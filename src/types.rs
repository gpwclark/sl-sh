use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::iter;
use std::num::{ParseFloatError, ParseIntError};
use std::process::Child;
use std::rc::Rc;
//use std::sync::RwLock;

use crate::builtins_util::is_proper_list;
use crate::environment::*;
use crate::eval::call_lambda;
use crate::gc::*;
use crate::process::*;
use crate::trace::*;

/*pub type GC = Heap;
static mut STATIC_GC: Option<GcWrap> = None;

pub fn gc<'a>() -> GcWrapRef<'a, ExpObj> {
    unsafe {
        if let Some(gc) = &STATIC_GC {
    gc.get()
        } else {
    panic!("GC not initialized!")
        }
    }
}

//pub fn gc_mut<'a>() -> &'a mut GC {
pub fn gc_mut<'a>() -> GcWrapRefMut<'a, ExpObj> {
    unsafe {
        if let Some(gc) = &STATIC_GC {
    gc.get_mut()
        } else {
    panic!("GC not initialized!")
        }
    }
}

pub fn init_gc() {
    unsafe {
        if STATIC_GC.is_none() {
            STATIC_GC = Some(create_gc::());
        }
    }
}
*/
#[derive(Clone, Debug)]
pub struct Lambda {
    pub params: Handle,
    pub body: Handle,
    pub capture: Rc<RefCell<Scope>>,
}

#[derive(Clone, Debug)]
pub struct Macro {
    pub params: Handle,
    pub body: Handle,
}

#[derive(Clone, Debug)]
pub enum Atom {
    True,
    Float(f64),
    Int(i64),
    Symbol(&'static str),
    StringRef(&'static str),
    String(String),
    StringBuf(Rc<RefCell<String>>),
    Char(char),
    Lambda(Lambda),
    Macro(Macro),
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Atom::True => write!(f, "true"),
            Atom::Float(n) => write!(f, "{}", n),
            Atom::Int(i) => write!(f, "{}", i),
            Atom::Symbol(s) => write!(f, "{}", s),
            Atom::StringRef(s) => write!(f, "\"{}\"", s),
            Atom::String(s) => write!(f, "\"{}\"", s),
            Atom::StringBuf(s) => write!(f, "\"{}\"", s.borrow()),
            Atom::Char(c) => write!(f, "#\\{}", c),
            Atom::Lambda(l) => {
                let params: Expression = l.params.clone().into();
                let body: Expression = l.body.clone().into();
                write!(f, "(fn {} {})", params.to_string(), body.to_string())
            }
            Atom::Macro(m) => {
                let params: Expression = m.params.clone().into();
                let body: Expression = m.body.clone().into();
                write!(f, "(macro {} {})", params.to_string(), body.to_string())
            }
        }
    }
}

impl Atom {
    // Like to_string but don't put quotes around strings or #\ in front of chars.
    pub fn as_string(&self) -> String {
        if let Atom::StringRef(s) = self {
            (*s).to_string()
        } else if let Atom::String(s) = self {
            s.to_string()
        } else if let Atom::StringBuf(s) = self {
            s.borrow().to_string()
        } else if let Atom::Char(c) = self {
            c.to_string()
        } else {
            self.to_string()
        }
    }

    pub fn display_type(&self) -> String {
        match self {
            Atom::True => "True".to_string(),
            Atom::Float(_) => "Float".to_string(),
            Atom::Int(_) => "Int".to_string(),
            Atom::Symbol(_) => "Symbol".to_string(),
            Atom::String(_) => "String".to_string(),
            Atom::StringRef(_) => "String".to_string(),
            Atom::StringBuf(_) => "StringBuf".to_string(),
            Atom::Char(_) => "Char".to_string(),
            Atom::Lambda(_) => "Lambda".to_string(),
            Atom::Macro(_) => "Macro".to_string(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum ProcessState {
    Running(u32),   // pid
    Over(u32, i32), // pid and exit status
}

pub enum FileState {
    Stdin,
    Stdout,
    Stderr,
    Read(RefCell<BufReader<File>>),
    Write(RefCell<BufWriter<File>>),
    Closed,
}

pub struct PairIter {
    current: Option<Expression>,
}

impl PairIter {
    fn new(exp: Expression) -> PairIter {
        PairIter { current: Some(exp) }
    }
}

impl Iterator for PairIter {
    type Item = Expression;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(current) = self.current.clone() {
            if let ExpEnum::Pair(e1, e2) = &current.get().data {
                let e1: Expression = e1.into();
                let e2: Expression = e2.into();
                self.current = Some(e2.clone());
                Some(e1.clone())
            } else {
                None
            }
        } else {
            None
        }
    }
}

pub struct ListIter<'a> {
    inner_iter: std::slice::Iter<'a, Handle>,
}

impl<'a> ListIter<'a> {
    pub fn new_list(list: &[Handle]) -> ListIter<'_> {
        ListIter {
            inner_iter: list.iter(),
        }
    }

    pub fn new_slice(list: &'a [Handle]) -> ListIter<'a> {
        ListIter {
            inner_iter: list.iter(),
        }
    }
}

impl<'a> Iterator for ListIter<'a> {
    type Item = Expression;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(exp) = self.inner_iter.next() {
            let exp: Expression = exp.into();
            Some(exp.clone())
        } else {
            None
        }
    }
}

type CallFunc =
    fn(&mut Environment, &mut dyn Iterator<Item = Expression>) -> io::Result<Expression>;

#[derive(Clone)]
pub struct Callable {
    pub func: CallFunc,
    pub is_special_form: bool,
}

impl Callable {
    pub fn new(func: CallFunc, is_special_form: bool) -> Callable {
        Callable {
            func,
            is_special_form,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ExpMeta {
    pub file: &'static str,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone)]
pub enum ExpEnum {
    Atom(Atom),
    Vector(Vec<Handle>),
    Pair(Handle, Handle),
    HashMap(HashMap<String, Handle>),
    Function(Callable),
    Process(ProcessState),
    File(Rc<RefCell<FileState>>),
    LazyFn(Lambda, Vec<Handle>),
    Nil,
}

impl ExpEnum {
    pub fn replace(&mut self, new_data: ExpEnum) {
        *self = new_data;
    }

    pub fn cons_from_vec(v: &mut Vec<Handle>) -> ExpEnum {
        let mut last_pair = ExpEnum::Nil;
        if !v.is_empty() {
            let mut i = v.len() - 1;
            loop {
                if i == 0 {
                    last_pair = ExpEnum::Pair(
                        v.remove(i),
                        Expression::alloc(ExpObj {
                            data: last_pair.clone(),
                            meta: None,
                        })
                        .into(),
                    );
                    break;
                } else {
                    last_pair = ExpEnum::Pair(
                        v.remove(i),
                        Expression::alloc(ExpObj {
                            data: last_pair.clone(),
                            meta: None,
                        })
                        .into(),
                    );
                }
                i -= 1;
            }
        }
        last_pair
    }
}

#[derive(Clone)]
pub struct ExpObj {
    pub data: ExpEnum,
    pub meta: Option<ExpMeta>,
}

impl Trace for ExpEnum {
    fn trace(&self, tracer: &mut Tracer) {
        match self {
            Self::Vector(list) => list.trace(tracer),
            Self::Pair(p1, p2) => {
                p1.trace(tracer);
                p2.trace(tracer);
            }
            Self::HashMap(map) => map.trace(tracer),
            Self::LazyFn(lambda, exp) => {
                lambda.params.trace(tracer);
                lambda.body.trace(tracer);
                exp.trace(tracer);
            }
            Self::Atom(Atom::Lambda(lambda)) => {
                lambda.params.trace(tracer);
                lambda.body.trace(tracer);
            }
            Self::Atom(Atom::Macro(mac)) => {
                mac.params.trace(tracer);
                mac.body.trace(tracer);
            }
            _ => {}
        }
    }
}

impl Trace for ExpObj {
    fn trace(&self, tracer: &mut Tracer) {
        self.data.trace(tracer);
    }
}

#[derive(Clone)]
pub struct Expression {
    obj: Handle,

    cache: Rc<RefCell<Option<GcRef>>>,
}

impl Expression {
    pub fn new(obj: Handle) -> Expression {
        Expression {
            obj,
            cache: Rc::new(RefCell::new(None)),
        }
    }

    pub fn alloc_h(obj: ExpObj) -> Handle {
        gc_mut().insert(obj)
    }

    pub fn alloc_data_h(data: ExpEnum) -> Handle {
        gc_mut().insert(ExpObj { data, meta: None })
    }

    pub fn alloc(obj: ExpObj) -> Expression {
        Expression::alloc_h(obj).into()
    }

    pub fn alloc_data(data: ExpEnum) -> Expression {
        Expression::alloc_data_h(data).into()
    }

    pub fn make_nil_h() -> Handle {
        gc_mut().insert(ExpObj {
            data: ExpEnum::Nil,
            meta: None,
        })
    }

    pub fn make_true_h() -> Handle {
        gc_mut().insert(ExpObj {
            data: ExpEnum::Atom(Atom::True),
            meta: None,
        })
    }

    pub fn make_nil() -> Expression {
        Expression::make_nil_h().into()
    }

    pub fn make_true() -> Expression {
        Expression::make_true_h().into()
    }

    pub fn handle_no_root(&self) -> Handle {
        self.obj.clone_no_root()
    }

    pub fn duplicate(&self) -> Expression {
        Expression::alloc(self.get().clone())
    }

    pub fn with<F, R>(&self, callback: F) -> Option<R>
    where
        F: FnOnce(&ExpObj) -> R,
    {
        self.obj.with(callback)
    }

    pub fn invalidate(&self) {
        self.cache.replace(None);
    }

    //pub fn get(&self) -> GcRef<'_> {
    pub fn get(&self) -> GcRef {
        //self.obj.get().expect("Invalid expression!")
        //let cache = self.cache.borrow();
        if self.cache.borrow().is_none() {
            let val = self.obj.get().expect("Invalid expression!");
            self.cache.replace(Some(val));
        }
        GcRef::clone(self.cache.borrow().as_ref().expect("Invalid expression, missing cache!"))
        /*if let Some(r) = cache.as_ref() {
            r
        } else {
            panic!("Invalid expression, missing cache!");
        }*/
    }

    pub fn get_mut(&self) -> GcRefMut {
        self.invalidate();
        self.obj.get_mut().expect("Invalid mut expression!")
        //gc().get_mut(&self.obj).expect("Invalid expression!")
    }

    pub fn clone_root(&self) -> Expression {
        Expression {
            obj: self.obj.clone_root(),
            cache: self.cache.clone(),
        }
    }

    pub fn meta(&self) -> Option<ExpMeta> {
        self.get().meta.clone()
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = Expression>> {
        let data = self.get();
        match &data.data {
            ExpEnum::Pair(_, _) => Box::new(PairIter::new(self.clone())),
            ExpEnum::Vector(_) => panic!("Can not make a vector iterator this way!"),
            _ => Box::new(iter::empty()),
        }
    }

    pub fn is_nil(&self) -> bool {
        let data = &self.get().data;
        if let ExpEnum::Nil = data {
            true
        } else {
            false
        }
    }

    // If the expression is a lazy fn then resolve it to concrete expression.
    pub fn resolve(&self, environment: &mut Environment) -> io::Result<Self> {
        if let ExpEnum::LazyFn(lambda, parts) = &self.get().data {
            let ib = &mut Box::new(ListIter::new_list(parts));
            let res = call_lambda(environment, &lambda, ib, false)?;
            res.resolve(environment)
        } else {
            // XXX clone to a root..
            Ok(self.clone())
        }
    }

    pub fn make_function(func: CallFunc, doc_str: &str, namespace: &'static str) -> Reference {
        Reference::new(
            ExpEnum::Function(Callable::new(func, false)),
            RefMetaData {
                namespace: Some(namespace),
                doc_string: Some(doc_str.to_string()),
            },
        )
    }

    pub fn make_special(func: CallFunc, doc_str: &str, namespace: &'static str) -> Reference {
        Reference::new(
            ExpEnum::Function(Callable::new(func, true)),
            RefMetaData {
                namespace: Some(namespace),
                doc_string: Some(doc_str.to_string()),
            },
        )
    }

    pub fn with_list(list: Vec<Handle>) -> Expression {
        Expression::alloc(ExpObj {
            data: ExpEnum::Vector(list),
            meta: None,
        })
    }

    pub fn with_list_meta(list: Vec<Handle>, meta: Option<ExpMeta>) -> Expression {
        Expression::alloc(ExpObj {
            data: ExpEnum::Vector(list),
            meta,
        })
    }

    pub fn cons_from_vec(v: &[Handle], meta: Option<ExpMeta>) -> Expression {
        let mut last_pair = ExpEnum::Nil;
        if !v.is_empty() {
            let mut i = v.len();
            while i > 0 {
                last_pair = ExpEnum::Pair(
                    v[i - 1].clone(),
                    Expression::alloc(ExpObj {
                        data: last_pair.clone(),
                        meta: None,
                    })
                    .into(),
                );
                i -= 1;
            }
        }
        Expression::alloc(ExpObj {
            data: last_pair,
            meta,
        })
    }

    pub fn display_type(&self) -> String {
        match &self.get().data {
            ExpEnum::Atom(a) => a.display_type(),
            ExpEnum::Process(_) => "Process".to_string(),
            ExpEnum::Function(f) => {
                if f.is_special_form {
                    "SpecialForm".to_string()
                } else {
                    "Function".to_string()
                }
            }
            ExpEnum::Vector(_) => "Vector".to_string(),
            ExpEnum::Pair(_, _) => "Pair".to_string(),
            ExpEnum::HashMap(_) => "HashMap".to_string(),
            ExpEnum::File(_) => "File".to_string(),
            ExpEnum::LazyFn(_, _) => "Lambda".to_string(),
            ExpEnum::Nil => "Nil".to_string(),
        }
    }

    fn pid_to_string(
        &self,
        procs: Rc<RefCell<HashMap<u32, Child>>>,
        pid: u32,
    ) -> io::Result<String> {
        match procs.borrow_mut().get_mut(&pid) {
            Some(child) => {
                if child.stdout.is_some() {
                    let mut buffer = String::new();
                    child.stdout.as_mut().unwrap().read_to_string(&mut buffer)?;
                    Ok(buffer)
                } else {
                    Ok("".to_string())
                }
            }
            None => Ok("".to_string()),
        }
    }

    fn pretty_print_int(
        &self,
        environment: &mut Environment,
        indent: usize,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        fn init_space(indent: usize, writer: &mut dyn Write) -> io::Result<()> {
            let mut i = 0;
            if indent > 0 {
                writer.write_all(b"\n")?;
            }
            while i < indent {
                writer.write_all(b"    ")?;
                i += 1;
            }
            Ok(())
        }
        match &self.get().data {
            ExpEnum::Vector(list) => {
                init_space(indent, writer)?;
                let a_str = self.to_string();
                if a_str.len() < 40 || a_str.starts_with('\'') || a_str.starts_with('`') {
                    writer.write_all(a_str.as_bytes())?;
                } else {
                    writer.write_all(b"#(")?;
                    let mut first = true;
                    for exp in list.iter() {
                        let exp: Expression = exp.into();
                        if !first {
                            writer.write_all(b" ")?;
                        } else {
                            first = false;
                        }
                        exp.pretty_print_int(environment, indent + 1, writer)?;
                    }
                    writer.write_all(b")")?;
                }
            }
            ExpEnum::Pair(e1, e2) => {
                init_space(indent, writer)?;
                let a_str = self.to_string();
                if a_str.len() < 40 || a_str.starts_with('\'') || a_str.starts_with('`') {
                    writer.write_all(a_str.as_bytes())?;
                } else if is_proper_list(&self) {
                    writer.write_all(b"(")?;
                    let mut first = true;
                    let mut last_p: Expression = Expression::make_nil().into();
                    for p in self.iter() {
                        if !first {
                            if let ExpEnum::Atom(Atom::Symbol(sym)) = &last_p.get().data {
                                if sym != &"," && sym != &",@" {
                                    writer.write_all(b" ")?;
                                }
                            } else {
                                writer.write_all(b" ")?;
                            }
                        } else {
                            first = false;
                        }
                        p.pretty_print_int(environment, indent + 1, writer)?;
                        last_p = p; //&p.get().data;
                    }
                    writer.write_all(b")")?;
                } else {
                    let e1: Expression = e1.into();
                    let e2: Expression = e2.into();
                    write!(writer, "({} . {})", e1.to_string(), e2.to_string())?;
                }
            }
            ExpEnum::Nil => write!(writer, "nil")?,
            ExpEnum::HashMap(map) => {
                init_space(indent, writer)?;
                let a_str = self.to_string();
                if a_str.len() < 40 {
                    writer.write_all(a_str.as_bytes())?;
                } else {
                    writer.write_all(b"(make-hash (")?;
                    for (key, val) in map.iter() {
                        let val: Expression = val.into();
                        init_space(indent + 1, writer)?;
                        write!(writer, "({} . {})", key, val)?;
                    }
                    write!(writer, "))")?;
                }
            }
            ExpEnum::Atom(Atom::String(_s)) => {
                write!(writer, "{}", self.to_string())?;
            }
            ExpEnum::Atom(Atom::StringBuf(_s)) => {
                write!(writer, "(str-buf {})", self.to_string())?;
            }
            ExpEnum::Atom(Atom::Char(_c)) => {
                write!(writer, "{}", self.to_string())?;
            }
            ExpEnum::Atom(Atom::Lambda(l)) => {
                let body: Expression = l.body.clone().into();
                let params: Expression = l.params.clone().into();
                write!(writer, "(fn {}", params.to_string())?;
                body.pretty_print_int(environment, indent + 1, writer)?;
                writer.write_all(b")")?;
            }
            ExpEnum::Atom(Atom::Macro(m)) => {
                let body: Expression = m.body.clone().into();
                let params: Expression = m.params.clone().into();
                write!(writer, "(macro {}", params.to_string())?;
                body.pretty_print_int(environment, indent + 1, writer)?;
                writer.write_all(b")")?;
            }
            _ => self.writef(environment, writer)?,
        }
        Ok(())
    }

    pub fn pretty_printf(
        &self,
        environment: &mut Environment,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        self.pretty_print_int(environment, 0, writer)
    }

    pub fn pretty_print(&self, environment: &mut Environment) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        self.pretty_print_int(environment, 0, &mut handle)
    }

    pub fn make_string(&self, environment: &Environment) -> io::Result<String> {
        match &self.get().data {
            ExpEnum::Atom(a) => Ok(a.to_string()),
            ExpEnum::Process(ProcessState::Running(_pid)) => Ok(self.to_string()),
            ExpEnum::Process(ProcessState::Over(pid, _exit_status)) => {
                self.pid_to_string(environment.procs.clone(), *pid)
            }
            ExpEnum::Function(_) => Ok(self.to_string()),
            ExpEnum::Vector(_) => Ok(self.to_string()),
            ExpEnum::Pair(_, _) => Ok(self.to_string()),
            ExpEnum::Nil => Ok(self.to_string()),
            ExpEnum::HashMap(_map) => Ok(self.to_string()),
            ExpEnum::File(file) => match &*file.borrow_mut() {
                FileState::Stdin => {
                    let f = io::stdin();
                    let mut f = f.lock();
                    let mut out_str = String::new();
                    f.read_to_string(&mut out_str)?;
                    Ok(out_str)
                }
                FileState::Read(f) => {
                    let mut out_str = String::new();
                    f.borrow_mut().read_to_string(&mut out_str)?;
                    Ok(out_str)
                }
                _ => Ok(self.to_string()),
            },
            ExpEnum::LazyFn(_, _) => Ok(self.to_string()),
        }
    }

    // Like make_string but don't put quotes around strings.
    pub fn as_string(&self, environment: &Environment) -> io::Result<String> {
        if let ExpEnum::Atom(a) = &self.get().data {
            Ok(a.as_string())
        } else {
            self.make_string(environment)
        }
    }

    pub fn make_float(&self, environment: &Environment) -> io::Result<f64> {
        match &self.get().data {
            ExpEnum::Atom(Atom::Float(f)) => Ok(*f),
            ExpEnum::Atom(Atom::Int(i)) => Ok(*i as f64),
            ExpEnum::Atom(_) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::Process(ProcessState::Running(_pid)) => Err(io::Error::new(
                io::ErrorKind::Other,
                "Not a number (process still running!)",
            )),
            ExpEnum::Process(ProcessState::Over(pid, _exit_status)) => {
                let buffer = self.pid_to_string(environment.procs.clone(), *pid)?;
                let potential_float: Result<f64, ParseFloatError> = buffer.parse();
                match potential_float {
                    Ok(v) => Ok(v),
                    Err(_) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
                }
            }
            ExpEnum::Function(_) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::Vector(_) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::Pair(_, _) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::Nil => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::HashMap(_) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::File(_) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
            ExpEnum::LazyFn(_, _) => Err(io::Error::new(io::ErrorKind::Other, "Not a number")),
        }
    }

    pub fn make_int(&self, environment: &Environment) -> io::Result<i64> {
        match &self.get().data {
            ExpEnum::Atom(Atom::Int(i)) => Ok(*i),
            ExpEnum::Atom(_) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::Process(ProcessState::Running(_pid)) => Err(io::Error::new(
                io::ErrorKind::Other,
                "Not an integer (process still running!)",
            )),
            ExpEnum::Process(ProcessState::Over(pid, _exit_status)) => {
                let buffer = self.pid_to_string(environment.procs.clone(), *pid)?;
                let potential_int: Result<i64, ParseIntError> = buffer.parse();
                match potential_int {
                    Ok(v) => Ok(v),
                    Err(_) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
                }
            }
            ExpEnum::Function(_) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::Vector(_) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::Pair(_, _) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::Nil => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::HashMap(_) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::File(_) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
            ExpEnum::LazyFn(_, _) => Err(io::Error::new(io::ErrorKind::Other, "Not an integer")),
        }
    }

    pub fn writef(&self, environment: &mut Environment, writer: &mut dyn Write) -> io::Result<()> {
        match &self.get().data {
            ExpEnum::Atom(a) => write!(writer, "{}", a.as_string())?,
            ExpEnum::Process(ps) => {
                let pid = match ps {
                    ProcessState::Running(pid) => pid,
                    ProcessState::Over(pid, _exit_status) => pid,
                };
                let procs = environment.procs.clone();
                let mut procs = procs.borrow_mut();
                match procs.get_mut(&pid) {
                    Some(child) => {
                        if child.stdout.is_some() {
                            let out = child.stdout.as_mut().unwrap();
                            let mut buf = [0; 1024];
                            loop {
                                match out.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => writer.write_all(&buf[..n])?,
                                    Err(err) => return Err(err),
                                }
                            }
                        } else {
                            return Err(io::Error::new(
                                io::ErrorKind::Other,
                                "Failed to get process out to write to.",
                            ));
                        }
                    }
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            "Failed to get process to write to.",
                        ));
                    }
                }
                drop(procs);
                wait_pid(environment, *pid, None);
            }
            ExpEnum::Function(_) => write!(writer, "{}", self.to_string())?,
            ExpEnum::Vector(_) => write!(writer, "{}", self.to_string())?,
            ExpEnum::Pair(_, _) => write!(writer, "{}", self.to_string())?,
            ExpEnum::Nil => write!(writer, "{}", self.to_string())?,
            ExpEnum::HashMap(_map) => write!(writer, "{}", self.to_string())?,
            ExpEnum::File(file) => match &*file.borrow_mut() {
                FileState::Stdin => {
                    let f = io::stdin();
                    let mut f = f.lock();
                    let mut buf = [0; 1024];
                    loop {
                        match f.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => writer.write_all(&buf[..n])?,
                            Err(err) => return Err(err),
                        }
                    }
                }
                FileState::Read(f) => {
                    let mut buf = [0; 1024];
                    loop {
                        match f.borrow_mut().read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => writer.write_all(&buf[..n])?,
                            Err(err) => return Err(err),
                        }
                    }
                }
                _ => write!(writer, "{}", self.to_string())?,
            },
            ExpEnum::LazyFn(_, _) => write!(writer, "{}", self.to_string())?,
        }
        writer.flush()?;
        Ok(())
    }

    pub fn write(&self, environment: &mut Environment) -> io::Result<()> {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        self.writef(environment, &mut handle)
    }
}

impl Trace for Expression {
    fn trace(&self, tracer: &mut Tracer) {
        self.obj.trace(tracer);
    }
}

impl Trace for [Expression] {
    fn trace(&self, tracer: &mut Tracer) {
        self.iter().for_each(|object| object.trace(tracer));
    }
}

impl<K> Trace for HashMap<K, Expression> {
    fn trace(&self, tracer: &mut Tracer) {
        self.values().for_each(|object| object.trace(tracer));
    }
}

impl Trace for [Handle] {
    fn trace(&self, tracer: &mut Tracer) {
        self.iter().for_each(|object| object.trace(tracer));
    }
}

impl<K> Trace for HashMap<K, Handle> {
    fn trace(&self, tracer: &mut Tracer) {
        self.values().for_each(|object| object.trace(tracer));
    }
}

impl AsRef<Handle> for Expression {
    fn as_ref(&self) -> &Handle {
        &self.obj
    }
}

impl AsRef<Expression> for Expression {
    fn as_ref(&self) -> &Expression {
        &self
    }
}

impl From<Expression> for Handle {
    fn from(item: Expression) -> Self {
        item.obj
    }
}

impl From<Handle> for Expression {
    fn from(item: Handle) -> Self {
        Expression::new(item)
    }
}

impl From<&Handle> for Expression {
    fn from(item: &Handle) -> Self {
        Expression::new(item.clone())
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fn list_out(res: &mut String, itr: &mut dyn Iterator<Item = Expression>) {
            let mut first = true;
            let mut last_exp: Expression = Expression::make_nil().into();
            for p in itr {
                if !first {
                    if let ExpEnum::Atom(Atom::Symbol(sym)) = &last_exp.get().data {
                        if sym != &"," && sym != &",@" {
                            res.push_str(" ");
                        }
                    } else {
                        res.push_str(" ");
                    }
                } else {
                    first = false;
                }
                res.push_str(&p.to_string());
                last_exp = p;
            }
        }

        match &self.get().data {
            ExpEnum::Atom(a) => write!(f, "{}", a),
            ExpEnum::Process(ProcessState::Running(pid)) => write!(f, "#<PID: {} Running>", pid),
            ExpEnum::Process(ProcessState::Over(pid, exit_status)) => write!(
                f,
                "#<PID: {}, EXIT STATUS: {},  Complete>",
                pid, exit_status
            ),
            ExpEnum::Function(_) => write!(f, "#<Function>"),
            ExpEnum::Vector(list) => {
                let mut res = String::new();
                res.push_str("#(");
                let mut ib = Box::new(ListIter::new_list(&list));
                list_out(&mut res, &mut ib);
                res.push(')');
                write!(f, "{}", res)
            }
            ExpEnum::Pair(e1, e2) => {
                let e1: Expression = e1.into();
                let e2: Expression = e2.into();
                if is_proper_list(&self) {
                    match &e1.get().data {
                        ExpEnum::Atom(Atom::Symbol(sym)) if sym == &"quote" => {
                            f.write_str("'")?;
                            // This will be a two element list or something is wrong...
                            if let ExpEnum::Pair(a2, _) = &e2.get().data {
                                let a2: Expression = a2.into();
                                f.write_str(&a2.to_string())
                            } else {
                                f.write_str(&e2.to_string())
                            }
                        }
                        ExpEnum::Atom(Atom::Symbol(sym)) if sym == &"bquote" => {
                            f.write_str("`")?;
                            // This will be a two element list or something is wrong...
                            if let ExpEnum::Pair(a2, _) = &e2.get().data {
                                let a2: Expression = a2.into();
                                f.write_str(&a2.to_string())
                            } else {
                                f.write_str(&e2.to_string())
                            }
                        }
                        _ => {
                            let mut res = String::new();
                            res.push_str("(");
                            list_out(&mut res, &mut self.iter());
                            res.push(')');
                            write!(f, "{}", res)
                        }
                    }
                } else {
                    let e1: Expression = e1.into();
                    let e2: Expression = e2.into();
                    write!(f, "({} . {})", e1.to_string(), e2.to_string())
                }
            }
            ExpEnum::Nil => f.write_str("nil"),
            ExpEnum::HashMap(map) => {
                let mut res = String::new();
                res.push_str("(make-hash (");
                for (key, val) in map.iter() {
                    let val: Expression = val.into();
                    res.push_str(&format!("({} . {})", key, val));
                }
                res.push_str("))");
                write!(f, "{}", res)
            }
            ExpEnum::File(file) => match &*file.borrow() {
                FileState::Stdout => write!(f, "#<STDOUT>"),
                FileState::Stderr => write!(f, "#<STDERR>"),
                FileState::Stdin => write!(f, "#<STDIN>"),
                FileState::Closed => write!(f, "#<CLOSED FILE>"),
                FileState::Read(_file) => write!(f, "#<READ FILE>"),
                FileState::Write(_file) => write!(f, "#<WRITE FILE>"),
            },
            ExpEnum::LazyFn(_, args) => {
                let mut res = String::new();
                res.push_str("#<LAZYFN<");
                list_out(&mut res, &mut Box::new(ListIter::new_list(args)));
                res.push_str(">>");
                write!(f, "{}", res)
            }
        }
    }
}

impl fmt::Debug for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.get().data {
            ExpEnum::Atom(a) => write!(f, "ExpEnum::Atom({:?})", a),
            ExpEnum::Vector(l) => write!(f, "ExpEnum::Vector({:?})", l),
            ExpEnum::Pair(e1, e2) => write!(f, "ExpEnum::Pair({:?} . {:?})", e1, e2),
            ExpEnum::HashMap(map) => write!(f, "ExpEnum::HashMap({:?})", map),
            ExpEnum::Function(_) => write!(f, "ExpEnum::Function(_)"),
            ExpEnum::Process(ProcessState::Running(pid)) => {
                write!(f, "ExpEnum::Process(ProcessStats::Running({}))", pid)
            }
            ExpEnum::Process(ProcessState::Over(pid, exit_status)) => write!(
                f,
                "ExpEnum::Process(ProcessState::Over({}, {}))",
                pid, exit_status
            ),
            ExpEnum::File(_) => write!(f, "ExpEnum::File(_)"),
            ExpEnum::LazyFn(_, exp) => write!(f, "ExpEnum::LazyFn({:?})", exp),
            ExpEnum::Nil => write!(f, "ExpEnum::Nil"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_one() {
        init_gc();
        let s1 = Expression::alloc_data_h(ExpEnum::Atom(Atom::String("sls".to_string())));
        let n1 = Expression::make_nil_h();
        let p1 = Expression::alloc_data(ExpEnum::Pair(s1.clone_no_root(), n1.clone_no_root()));
        let l1 = Expression::with_list(vec![
            Expression::make_nil_h().clone_no_root(),
            Expression::make_nil_h().clone_no_root(),
        ]);
        gc_mut().clean();
        //println!("XXX {}, {}, {}", p1, s1, n1);
        //println!("XXX {}", l1);
        //assert!(1 == 2);
    }
}
