use nix::{
    sys::{
        signal::{self, Signal},
        termios,
    },
    unistd::{self, Pid},
};
use std::collections::{hash_map, HashMap};
use std::env;
use std::fs;
use std::hash::BuildHasher;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

use crate::builtins_util::*;
use crate::config::VERSION_STRING;
use crate::environment::*;
use crate::eval::*;
use crate::interner::*;
use crate::process::*;
use crate::reader::*;
use crate::types::*;

fn builtin_eval(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg) = args.next() {
        if args.next().is_none() {
            let arg = eval(environment, &arg)?;
            return match arg {
                Expression::Atom(Atom::String(s)) => match read(environment, &s, None) {
                    Ok(ast) => eval(environment, &ast),
                    Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.reason)),
                },
                Expression::Atom(Atom::StringRef(s)) => match read(environment, s, None) {
                    Ok(ast) => eval(environment, &ast),
                    Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.reason)),
                },
                Expression::Atom(Atom::StringBuf(s)) => {
                    match read(environment, &s.borrow(), None) {
                        Ok(ast) => eval(environment, &ast),
                        Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.reason)),
                    }
                }
                _ => eval(environment, &arg),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "eval can only have one form",
    ))
}

fn builtin_fncall(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut call_list = Vec::new();
    for arg in args {
        call_list.push(arg.clone());
    }
    if call_list.is_empty() {
        return Err(io::Error::new(io::ErrorKind::Other, "fn_call: empty call"));
    }
    let command = eval(environment, &call_list[0])?;
    fn_call(environment, &command, Box::new(call_list[1..].iter()))
}

fn builtin_apply(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut call_list = Vec::new();
    let mut last_arg: Option<&Expression> = None;
    for arg in args {
        if let Some(a) = last_arg {
            call_list.push(a);
        }
        last_arg = Some(arg);
    }
    let tlist;
    let list_borrow;
    let last_evaled;
    if let Some(alist) = last_arg {
        last_evaled = eval(environment, alist)?;
        let itr = match last_evaled {
            Expression::Vector(list, _) => {
                tlist = list;
                list_borrow = tlist.borrow();
                Box::new(list_borrow.iter())
            }
            Expression::Pair(_, _) => last_evaled.iter(), // Includes Nil.
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "apply: last arg not a list",
                ))
            }
        };
        for a in itr {
            call_list.push(a);
        }
    }
    if call_list.is_empty() {
        return Err(io::Error::new(io::ErrorKind::Other, "apply: empty call"));
    }
    let command = eval(environment, &call_list[0])?;
    fn_call(
        environment,
        &command,
        Box::new(call_list[1..].iter().copied()),
    )
}

fn builtin_unwind_protect(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(protected) = args.next() {
        let result = eval(environment, protected);
        for a in args {
            if let Err(err) = eval(environment, a) {
                eprintln!(
                    "ERROR in unwind-protect cleanup form {}, {} will continue cleanup",
                    a, err
                );
            }
        }
        result
    } else {
        Ok(Expression::nil())
    }
}

fn builtin_err(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg) = args.next() {
        if args.next().is_none() {
            let arg = eval(environment, arg)?;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                arg.as_string(environment)?,
            ));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "err can only have one form",
    ))
}

pub fn load(environment: &mut Environment, file_name: &str) -> io::Result<Expression> {
    let core_lisp = include_bytes!("../lisp/core.lisp");
    let seq_lisp = include_bytes!("../lisp/seq.lisp");
    let shell_lisp = include_bytes!("../lisp/shell.lisp");
    let scripting_lisp = include_bytes!("../lisp/scripting.lisp");
    let endfix_lisp = include_bytes!("../lisp/endfix.lisp");
    let slsh_std_lisp = include_bytes!("../lisp/slsh-std.lisp");
    let slshrc = include_bytes!("../lisp/slshrc");
    let file_name = match expand_tilde(&file_name) {
        Some(f) => f,
        None => file_name.to_string(),
    };
    let file_path = if let Some(lp) = get_expression(environment, "*load-path*") {
        let vec_borrow;
        let p_itr = match &lp.exp {
            Expression::Vector(vec, _) => {
                vec_borrow = vec.borrow();
                Box::new(vec_borrow.iter())
            }
            _ => lp.exp.iter(),
        };
        let mut path_out = file_name.clone();
        for l in p_itr {
            let path_name = match l {
                Expression::Atom(Atom::Symbol(sym)) => Some((*sym).to_string()),
                Expression::Atom(Atom::String(s)) => Some(s.to_string()),
                Expression::Atom(Atom::StringRef(s)) => Some((*s).to_string()),
                Expression::Atom(Atom::StringBuf(s)) => Some(s.borrow().to_string()),
                _ => None,
            };
            if let Some(path_name) = path_name {
                let path_str = if path_name.ends_with('/') {
                    format!("{}{}", path_name, file_name)
                } else {
                    format!("{}/{}", path_name, file_name)
                };
                let path = Path::new(&path_str);
                if path.exists() {
                    path_out = path_str;
                    break;
                }
            }
        }
        path_out
    } else {
        file_name
    };
    let path = Path::new(&file_path);
    let file_name = Some(environment.interner.intern(&file_path));
    let ast = if path.exists() {
        let contents = fs::read_to_string(file_path)?;
        read_list_wrap(environment, &contents, file_name)
    } else {
        match &file_path[..] {
            "core.lisp" => {
                read_list_wrap(environment, &String::from_utf8_lossy(core_lisp), file_name)
            }
            "seq.lisp" => {
                read_list_wrap(environment, &String::from_utf8_lossy(seq_lisp), file_name)
            }
            "shell.lisp" => {
                read_list_wrap(environment, &String::from_utf8_lossy(shell_lisp), file_name)
            }
            "scripting.lisp" => {
                read_list_wrap(environment, &String::from_utf8_lossy(scripting_lisp), file_name)
            }
            "endfix.lisp" => read_list_wrap(
                environment,
                &String::from_utf8_lossy(endfix_lisp),
                file_name,
            ),
            "slsh-std.lisp" => read_list_wrap(
                environment,
                &String::from_utf8_lossy(slsh_std_lisp),
                file_name,
            ),
            "slshrc" => read_list_wrap(environment, &String::from_utf8_lossy(slshrc), file_name),
            _ => {
                let msg = format!("{} not found", file_path);
                return Err(io::Error::new(io::ErrorKind::Other, msg));
            }
        }
    };
    match ast {
        Ok(ast) => {
            let old_loose_syms = environment.loose_symbols;
            // Do not use loose symbols in scripts even if loading from the repl.
            environment.loose_symbols = false;
            let mut res = Expression::nil();
            match ast {
                Expression::Vector(list, _) => {
                    for l in list.borrow_mut().drain(..) {
                        res = eval(environment, &l)?;
                    }
                }
                Expression::Pair(_, _) => {
                    for l in ast.iter() {
                        res = eval(environment, &l)?;
                    }
                }
                _ => {
                    res = eval(environment, &ast)?;
                }
            }
            environment.loose_symbols = old_loose_syms;
            Ok(res)
        }
        Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.reason)),
    }
}

fn builtin_load(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg) = args.next() {
        if args.next().is_none() {
            let arg = eval(environment, arg)?;
            let file_name = arg.as_string(environment)?;
            return load(environment, &file_name);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "load needs one argument",
    ))
}

fn builtin_length(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg) = args.next() {
        if args.next().is_none() {
            let arg = eval(environment, arg)?;
            return match &arg {
                Expression::Atom(Atom::String(s)) => {
                    let mut i = 0;
                    // Need to walk the chars to get the length in utf8 chars not bytes.
                    for _ in s.chars() {
                        i += 1;
                    }
                    Ok(Expression::Atom(Atom::Int(i64::from(i))))
                }
                Expression::Atom(Atom::StringRef(s)) => {
                    let mut i = 0;
                    // Need to walk the chars to get the length in utf8 chars not bytes.
                    for _ in s.chars() {
                        i += 1;
                    }
                    Ok(Expression::Atom(Atom::Int(i64::from(i))))
                }
                Expression::Atom(Atom::StringBuf(s)) => {
                    let mut i = 0;
                    // Need to walk the chars to get the length in utf8 chars not bytes.
                    for _ in s.borrow().chars() {
                        i += 1;
                    }
                    Ok(Expression::Atom(Atom::Int(i64::from(i))))
                }
                Expression::Atom(_) => Ok(Expression::Atom(Atom::Int(1))),
                Expression::Vector(list, _) => {
                    Ok(Expression::Atom(Atom::Int(list.borrow().len() as i64)))
                }
                Expression::Pair(p, _) => {
                    if let Some((_e1, e2)) = &*p.borrow() {
                        let mut len = 0;
                        let mut e_next = e2.clone();
                        loop {
                            match e_next {
                                Expression::Pair(p, _) => {
                                    len += 1;
                                    if let Some((_e1, e2)) = &*p.borrow() {
                                        e_next = e2.clone();
                                    } else {
                                        // Nil
                                        break;
                                    }
                                }
                                _ => {
                                    len += 1;
                                    break;
                                }
                            }
                        }
                        Ok(Expression::Atom(Atom::Int(len)))
                    } else {
                        // Nil
                        Ok(Expression::Atom(Atom::Int(0)))
                    }
                }
                Expression::HashMap(map) => {
                    Ok(Expression::Atom(Atom::Int(map.borrow().len() as i64)))
                }
                _ => Ok(Expression::Atom(Atom::Int(0))),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "length takes one form",
    ))
}

fn builtin_if(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(if_form) = args.next() {
        if let Some(then_form) = args.next() {
            return if eval(environment, if_form)?.is_nil() {
                if let Some(else_form) = args.next() {
                    eval_nr(environment, else_form)
                } else {
                    Ok(Expression::nil())
                }
            } else {
                eval_nr(environment, then_form)
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "if needs exactly two or three expressions",
    ))
}

fn args_out(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
    add_newline: bool,
    pretty: bool,
    writer: &mut dyn Write,
) -> io::Result<()> {
    for a in args {
        let aa = eval(environment, a)?;
        // If we have a standalone string do not quote it...
        let pretty = match aa {
            Expression::Atom(Atom::String(_)) => false,
            Expression::Atom(Atom::StringRef(_)) => false,
            Expression::Atom(Atom::StringBuf(_)) => false,
            _ => pretty,
        };
        if pretty {
            aa.pretty_printf(environment, writer)?;
        } else {
            aa.writef(environment, writer)?;
        }
    }
    if add_newline {
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn print_to_oe(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
    add_newline: bool,
    pretty: bool,
    default_error: bool,
    key: &str,
) -> io::Result<()> {
    let out = get_expression(environment, key);
    match out {
        Some(out) => {
            if let Expression::File(f) = &out.exp {
                match &*f.borrow() {
                    FileState::Stdout => {
                        let stdout = io::stdout();
                        let mut out = stdout.lock();
                        args_out(environment, args, add_newline, pretty, &mut out)?;
                    }
                    FileState::Stderr => {
                        let stdout = io::stderr();
                        let mut out = stdout.lock();
                        args_out(environment, args, add_newline, pretty, &mut out)?;
                    }
                    FileState::Write(f) => {
                        // Don't call args_out here our we will buy a borrow error...
                        for a in args {
                            let aa = eval(environment, a)?;
                            // If we have a standalone string do not quote it...
                            let pretty = match aa {
                                Expression::Atom(Atom::String(_)) => false,
                                Expression::Atom(Atom::StringRef(_)) => false,
                                Expression::Atom(Atom::StringBuf(_)) => false,
                                _ => pretty,
                            };
                            if pretty {
                                aa.pretty_printf(environment, &mut *f.borrow_mut())?;
                            } else {
                                aa.writef(environment, &mut *f.borrow_mut())?;
                            }
                        }
                        if add_newline {
                            (&mut *f.borrow_mut()).write_all(b"\n")?;
                        }
                    }
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            "ERROR: Can not print to a non-writable file.",
                        ));
                    }
                }
            } else {
                let msg = format!("ERROR: {} is not a file!", key);
                return Err(io::Error::new(io::ErrorKind::Other, msg));
            }
        }
        None => {
            if default_error {
                let stdout = io::stderr();
                let mut out = stdout.lock();
                args_out(environment, args, add_newline, pretty, &mut out)?;
            } else {
                let stdout = io::stdout();
                let mut out = stdout.lock();
                args_out(environment, args, add_newline, pretty, &mut out)?;
            }
        }
    }
    Ok(())
}

fn print(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
    add_newline: bool,
) -> io::Result<Expression> {
    match &environment.state.stdout_status {
        Some(IOState::Null) => { /* Nothing to do... */ }
        _ => {
            print_to_oe(environment, args, add_newline, true, false, "*stdout*")?;
        }
    };
    Ok(Expression::nil())
}

pub fn eprint(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
    add_newline: bool,
) -> io::Result<Expression> {
    match &environment.state.stderr_status {
        Some(IOState::Null) => { /* Nothing to do... */ }
        _ => {
            print_to_oe(environment, args, add_newline, true, true, "*stderr*")?;
        }
    };
    Ok(Expression::nil())
}

fn builtin_print(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    print(environment, args, false)
}

fn builtin_println(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    print(environment, args, true)
}

fn builtin_eprint(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    eprint(environment, args, false)
}

fn builtin_eprintln(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    eprint(environment, args, true)
}

fn builtin_format(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut res = String::new();
    for a in args {
        res.push_str(&eval(environment, a)?.as_string(environment)?);
    }
    Ok(Expression::Atom(Atom::String(res)))
}

pub fn builtin_progn(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut ret = Expression::nil();
    for arg in args {
        ret.resolve(environment)?;
        ret = eval_nr(environment, &arg)?;
    }
    Ok(ret)
}

fn proc_set_vars<'a>(
    environment: &mut Environment,
    args: &'a mut dyn Iterator<Item = &Expression>,
) -> io::Result<(&'static str, Option<String>, &'a Expression)> {
    if let Some(key) = args.next() {
        if let Some(arg1) = args.next() {
            let key = match eval(environment, key)? {
                Expression::Atom(Atom::Symbol(s)) => s,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "first form (binding key) must evaluate to a symbol",
                    ));
                }
            };
            if let Some(arg2) = args.next() {
                if args.next().is_none() {
                    let doc_str = if let Ok(docs) = eval(environment, arg1)?.as_string(environment)
                    {
                        Some(docs)
                    } else {
                        None
                    };
                    return Ok((key, doc_str, arg2));
                }
            } else {
                return Ok((key, None, arg1));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "def/set requires a key, optional docstring and value",
    ))
}

fn val_to_reference(
    environment: &mut Environment,
    namespace: Option<&'static str>,
    doc_string: Option<String>,
    val_in: &Expression,
) -> io::Result<(Rc<Reference>, Expression)> {
    if let Expression::Atom(Atom::Symbol(s)) = val_in {
        if let Some(exp) = get_expression(environment, s) {
            Ok((exp, eval(environment, val_in)?))
        } else {
            let val = eval(environment, &val_in)?;
            Ok((
                Rc::new(Reference {
                    exp: val.clone(),
                    meta: RefMetaData {
                        namespace,
                        doc_string,
                    },
                }),
                val,
            ))
        }
    } else {
        let val = eval(environment, val_in)?;
        Ok((
            Rc::new(Reference {
                exp: val.clone(),
                meta: RefMetaData {
                    namespace,
                    doc_string,
                },
            }),
            val,
        ))
    }
}

fn builtin_set(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let (key, doc_str, val) = proc_set_vars(environment, args)?;
    if let hash_map::Entry::Occupied(mut entry) = environment.dynamic_scope.entry(key) {
        // XXX TODO, eval val here?
        entry.insert(Rc::new(val.clone()));
        Ok(val.clone())
    } else if let Some(scope) = get_symbols_scope(environment, &key) {
        let name = scope.borrow().name;
        let (reference, val) = val_to_reference(environment, name, doc_str, val)?;
        scope.borrow_mut().data.insert(key, reference);
        Ok(val)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "set's first form must evaluate to an existing symbol",
        ))
    }
}

fn builtin_export(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(key) = args.next() {
        if let Some(val) = args.next() {
            if args.next().is_none() {
                let key = eval(environment, key)?;
                let val = eval(environment, val)?;
                let key = match key {
                    Expression::Atom(Atom::Symbol(s)) => s,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            "export: first form must evaluate to a symbol",
                        ));
                    }
                };
                let val = match &val {
                    Expression::Atom(Atom::Symbol(s)) => Expression::Atom(Atom::StringRef(s)),
                    Expression::Atom(Atom::StringRef(s)) => Expression::Atom(Atom::StringRef(s)),
                    Expression::Atom(Atom::String(s)) => {
                        Expression::Atom(Atom::String(s.to_string()))
                    }
                    Expression::Atom(Atom::StringBuf(s)) => {
                        Expression::Atom(Atom::String(s.borrow().clone()))
                    }
                    Expression::Atom(Atom::Int(i)) => {
                        Expression::Atom(Atom::String(format!("{}", i)))
                    }
                    Expression::Atom(Atom::Float(f)) => {
                        Expression::Atom(Atom::String(format!("{}", f)))
                    }
                    Expression::Process(ProcessState::Running(_pid)) => {
                        Expression::Atom(Atom::String(
                            val.as_string(environment)
                                .unwrap_or_else(|_| "PROCESS FAILED".to_string()),
                        ))
                    }
                    Expression::Process(ProcessState::Over(_pid, _exit_status)) => {
                        Expression::Atom(Atom::String(
                            val.as_string(environment)
                                .unwrap_or_else(|_| "PROCESS FAILED".to_string()),
                        ))
                    }
                    Expression::File(file) => match &*file.borrow() {
                        FileState::Stdin => Expression::Atom(Atom::String(
                            val.as_string(environment)
                                .unwrap_or_else(|_| "STDIN FAILED".to_string()),
                        )),
                        FileState::Read(_) => Expression::Atom(Atom::String(
                            val.as_string(environment)
                                .unwrap_or_else(|_| "FILE READ FAILED".to_string()),
                        )),
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::Other,
                                "export: value not valid",
                            ))
                        }
                    },
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            "export: value not valid",
                        ));
                    }
                };
                let val = val.as_string(environment)?;
                let val = match expand_tilde(&val) {
                    Some(v) => v,
                    None => val,
                };
                if !val.is_empty() {
                    env::set_var(key, val.clone());
                } else {
                    env::remove_var(key);
                }
                return Ok(Expression::Atom(Atom::String(val)));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "export: can only have two expressions",
    ))
}

fn builtin_unexport(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(key) = args.next() {
        if args.next().is_none() {
            let key = eval(environment, key)?;
            if let Expression::Atom(Atom::Symbol(k)) = key {
                env::remove_var(k);
                return Ok(Expression::nil());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "unexport can only have one expression (symbol)",
    ))
}

fn builtin_def(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    fn current_namespace(environment: &mut Environment) -> Option<&'static str> {
        if let Some(exp) = get_expression(environment, "*ns*") {
            match &exp.exp {
                Expression::Atom(Atom::String(s)) => Some(environment.interner.intern(s)),
                Expression::Atom(Atom::StringRef(s)) => Some(s),
                Expression::Atom(Atom::StringBuf(s)) => {
                    Some(environment.interner.intern(&*s.borrow()))
                }
                _ => None,
            }
        } else {
            None
        }
    }
    let (key, doc_string, val) = proc_set_vars(environment, args)?;
    if key.contains("::") {
        // namespace reference.
        let mut key_i = key.splitn(2, "::");
        if let Some(namespace) = key_i.next() {
            if let Some(key) = key_i.next() {
                let namespace = if namespace == "ns" {
                    current_namespace(environment)
                        .unwrap_or_else(|| environment.interner.intern("NO_NAME"))
                } else {
                    namespace
                };
                let mut scope = Some(environment.current_scope.last().unwrap().clone());
                while let Some(in_scope) = scope {
                    let name = in_scope.borrow().name;
                    if let Some(name) = name {
                        if name == namespace {
                            let (reference, val) =
                                val_to_reference(environment, Some(name), doc_string, val)?;
                            in_scope.borrow_mut().data.insert(key, reference);
                            return Ok(val);
                        }
                    }
                    scope = in_scope.borrow().outer.clone();
                }
            }
        }
        let msg = format!(
            "def namespaced symbol {} not valid or namespace not a parent namespace",
            key
        );
        Err(io::Error::new(io::ErrorKind::Other, msg))
    } else {
        let ns = current_namespace(environment);
        let (reference, val) = val_to_reference(environment, ns, doc_string, val)?;
        set_expression_current_ref(environment, key, reference);
        Ok(val)
    }
}

fn builtin_undef(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(key) = args.next() {
        if args.next().is_none() {
            let key = eval(environment, key)?;
            if let Expression::Atom(Atom::Symbol(k)) = key {
                remove_expression_current(environment, &k);
                return Ok(Expression::nil());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "undef can only have one expression (symbol)",
    ))
}

fn builtin_dyn(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let (key, val) = if let Some(key) = args.next() {
        if let Some(val) = args.next() {
            let key = match eval(environment, key)? {
                Expression::Atom(Atom::Symbol(s)) => s,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "first form (binding key) must evaluate to a symbol",
                    ));
                }
            };
            let val = eval(environment, val)?;
            (key, val)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "dyn requires a key and value",
            ));
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "dyn requires a key and value",
        ));
    };
    let old_val = if environment.dynamic_scope.contains_key(&key) {
        Some(environment.dynamic_scope.remove(&key).unwrap())
    } else {
        None
    };
    if let Some(exp) = args.next() {
        environment.dynamic_scope.insert(key, Rc::new(val));
        let res = eval(environment, exp);
        if let Some(old_val) = old_val {
            environment.dynamic_scope.insert(key, old_val);
        } else {
            environment.dynamic_scope.remove(&key);
        }
        res
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "dyn requires three expressions (symbol, value, form to evaluate)",
        ))
    }
}

fn builtin_to_symbol(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            let arg0 = eval(environment, arg0)?;
            return match &arg0 {
                Expression::Atom(Atom::String(s)) => Ok(Expression::Atom(Atom::Symbol(
                    environment.interner.intern(s),
                ))),
                Expression::Atom(Atom::StringRef(s)) => Ok(Expression::Atom(Atom::Symbol(s))),
                Expression::Atom(Atom::StringBuf(s)) => Ok(Expression::Atom(Atom::Symbol(
                    environment.interner.intern(&s.borrow()),
                ))),
                Expression::Atom(Atom::Symbol(s)) => Ok(Expression::Atom(Atom::Symbol(s))),
                Expression::Atom(Atom::Int(i)) => Ok(Expression::Atom(Atom::Symbol(
                    environment.interner.intern(&format!("{}", i)),
                ))),
                Expression::Atom(Atom::Float(f)) => Ok(Expression::Atom(Atom::Symbol(
                    environment.interner.intern(&format!("{}", f)),
                ))),
                _ => Err(io::Error::new(
                    io::ErrorKind::Other,
                    "to-symbol can only convert strings, symbols, ints and floats to a symbol",
                )),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "to-symbol take one form",
    ))
}

fn builtin_symbol_name(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            let arg0 = eval(environment, arg0)?;
            return match &arg0 {
                Expression::Atom(Atom::Symbol(s)) => Ok(Expression::Atom(Atom::StringRef(s))),
                _ => Err(io::Error::new(
                    io::ErrorKind::Other,
                    "symbol-name can only convert a symbol to a string",
                )),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "symbol-name take one form",
    ))
}

fn builtin_fn(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(params) = args.next() {
        if let Some(body) = args.next() {
            if args.next().is_none() {
                return Ok(Expression::Atom(Atom::Lambda(Lambda {
                    params: Box::new(params.clone()),
                    body: Box::new(body.clone()),
                    capture: environment.current_scope.last().unwrap().clone(),
                })));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "fn can only have two forms",
    ))
}

fn builtin_quote(
    _environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg) = args.next() {
        if args.next().is_none() {
            return Ok(arg.clone());
        }
    }
    Err(io::Error::new(io::ErrorKind::Other, "quote takes one form"))
}

fn replace_commas(
    environment: &mut Environment,
    list: &mut dyn Iterator<Item = &Expression>,
    is_vector: bool,
    meta: Option<ExpMeta>,
) -> io::Result<Expression> {
    let mut output: Vec<Expression> = Vec::new(); //with_capacity(list.len());
    let mut comma_next = false;
    let mut amp_next = false;
    for exp in list {
        let exp = match exp {
            Expression::Vector(tlist, m) => replace_commas(
                environment,
                &mut tlist.borrow().iter(),
                is_vector,
                m.clone(),
            )?,
            Expression::Pair(_, m) => {
                replace_commas(environment, &mut exp.iter(), is_vector, m.clone())?
            }
            _ => exp.clone(),
        };
        if let Expression::Atom(Atom::Symbol(symbol)) = &exp {
            if symbol == &"," {
                comma_next = true;
            } else if symbol == &",@" {
                amp_next = true;
            } else if comma_next {
                output.push(eval(environment, &exp)?);
                comma_next = false;
            } else if amp_next {
                let nl = eval(environment, &exp)?;
                if let Expression::Vector(new_list, _) = nl {
                    for item in new_list.borrow().iter() {
                        output.push(item.clone());
                    }
                } else if let Expression::Pair(_, _) = nl {
                    for item in nl.iter() {
                        output.push(item.clone());
                    }
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        ",@ must be applied to a list",
                    ));
                }
                amp_next = false;
            } else {
                output.push(exp);
            }
        } else if comma_next {
            output.push(eval(environment, &exp)?);
            comma_next = false;
        } else if amp_next {
            let nl = eval(environment, &exp)?;
            if let Expression::Vector(new_list, _) = nl {
                for item in new_list.borrow_mut().drain(..) {
                    output.push(item);
                }
            } else if let Expression::Pair(_, _) = nl {
                for item in nl.iter() {
                    output.push(item.clone());
                }
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    ",@ must be applied to a list",
                ));
            }
            amp_next = false;
        } else {
            output.push(exp);
        }
    }
    if is_vector {
        Ok(Expression::with_list_meta(output, meta))
    } else {
        Ok(Expression::cons_from_vec(&mut output, meta))
    }
}

fn builtin_bquote(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let ret = if let Some(arg) = args.next() {
        match arg {
            Expression::Atom(Atom::Symbol(s)) if s == &"," => {
                if let Some(exp) = args.next() {
                    Ok(eval(environment, exp)?)
                } else {
                    Ok(Expression::nil())
                }
            }
            Expression::Vector(list, meta) => replace_commas(
                environment,
                &mut Box::new(list.borrow().iter()),
                true,
                meta.clone(),
            ),
            Expression::Pair(p, meta) => {
                if let Some((_, _)) = &*p.borrow() {
                    replace_commas(environment, &mut arg.iter(), false, meta.clone())
                } else {
                    // Nil
                    Ok(arg.clone())
                }
            }
            _ => Ok(arg.clone()),
        }
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "bquote takes one form",
        ))
    };
    if args.next().is_some() {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "bquote takes one form",
        ))
    } else {
        ret
    }
}

/*fn builtin_spawn(environment: &mut Environment, args: &[Expression]) -> io::Result<Expression> {
    let mut new_args: Vec<Expression> = Vec::with_capacity(args.len());
    for a in args {
        new_args.push(a.clone());
    }
    let mut data: HashMap<String, Expression> = HashMap::new();
    clone_symbols(
        &environment.current_scope.last().unwrap().borrow(),
        &mut data,
    );
    let _child = std::thread::spawn(move || {
        let mut enviro = build_new_spawn_scope(data, environment.sig_int);
        let _args = to_args(&mut enviro, &new_args).unwrap();
        if let Err(err) = reap_procs(&enviro) {
            eprintln!("Error waiting on spawned processes: {}", err);
        }
    });
    //let res = child.join()
    Ok(Expression::nil())
}*/

fn builtin_and(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut last_exp = Expression::Atom(Atom::True);
    for arg in args {
        let arg = eval(environment, &arg)?;
        if arg.is_nil() {
            return Ok(Expression::nil());
        } else {
            last_exp = arg;
        }
    }
    Ok(last_exp)
}

fn builtin_or(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    for arg in args {
        let arg = eval(environment, &arg)?;
        if !arg.is_nil() {
            return Ok(arg);
        }
    }
    Ok(Expression::nil())
}

fn builtin_not(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            let arg0 = eval(environment, arg0)?;
            return if arg0.is_nil() {
                Ok(Expression::Atom(Atom::True))
            } else {
                Ok(Expression::nil())
            };
        }
    }
    Err(io::Error::new(io::ErrorKind::Other, "not takes one form"))
}

fn builtin_is_def(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    fn get_ret(environment: &mut Environment, name: &str) -> io::Result<Expression> {
        if is_expression(environment, name) {
            Ok(Expression::Atom(Atom::True))
        } else {
            Ok(Expression::nil())
        }
    }
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            let arg0 = eval(environment, arg0)?;
            return match arg0 {
                Expression::Atom(Atom::Symbol(s)) => get_ret(environment, s),
                Expression::Atom(Atom::StringRef(s)) => get_ret(environment, s),
                Expression::Atom(Atom::String(s)) => get_ret(environment, &s),
                Expression::Atom(Atom::StringBuf(s)) => get_ret(environment, &s.borrow()),
                _ => Err(io::Error::new(
                    io::ErrorKind::Other,
                    "def? takes a symbol or string (will be treated as a symbol) to lookup",
                )),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "def? takes one form (symbol or string)",
    ))
}

fn builtin_macro(
    _environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(params) = args.next() {
        if let Some(body) = args.next() {
            if args.next().is_none() {
                return Ok(Expression::Atom(Atom::Macro(Macro {
                    params: Box::new(params.clone()),
                    body: Box::new(body.clone()),
                })));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "macro can only have two forms (bindings and body)",
    ))
}

fn do_expansion(
    environment: &mut Environment,
    command: &Expression,
    parts: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Option<Expression>> {
    if let Expression::Atom(Atom::Symbol(command)) = command {
        if let Some(exp) = get_expression(environment, &command) {
            if let Expression::Atom(Atom::Macro(sh_macro)) = &exp.exp {
                let new_scope = match environment.current_scope.last() {
                    Some(last) => build_new_scope(Some(last.clone())),
                    None => build_new_scope(None),
                };
                environment.current_scope.push(new_scope);
                let args: Vec<Expression> = parts.cloned().collect();
                let ib: Box<(dyn Iterator<Item = &Expression>)> = Box::new(args.iter());
                if let Err(err) = setup_args(environment, None, &sh_macro.params, ib, false) {
                    environment.current_scope.pop();
                    return Err(err);
                }
                let expansion = eval(environment, &sh_macro.body);
                if let Err(err) = expansion {
                    environment.current_scope.pop();
                    return Err(err);
                }
                let expansion = expansion.unwrap();
                environment.current_scope.pop();
                Ok(Some(expansion))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

fn expand_macro_internal(
    environment: &mut Environment,
    arg: &Expression,
    one: bool,
) -> io::Result<Option<Expression>> {
    if let Expression::Vector(list, _) = arg {
        let list = list.borrow();
        let (command, parts) = match list.split_first() {
            Some((c, p)) => (c, p),
            None => {
                return Ok(None);
            }
        };
        let expansion = do_expansion(environment, command, &mut parts.iter())?;
        if let Some(expansion) = expansion {
            if !one {
                if let Some(new_expansion) = expand_macro(environment, &expansion, one)? {
                    Ok(Some(new_expansion))
                } else {
                    Ok(Some(expansion))
                }
            } else {
                Ok(Some(expansion))
            }
        } else {
            Ok(None)
        }
    } else if let Expression::Pair(p, _) = arg {
        if let Some((e1, e2)) = &*p.borrow() {
            let expansion = do_expansion(environment, &e1, &mut *e2.iter())?;
            if let Some(expansion) = expansion {
                if !one {
                    if let Some(new_expansion) = expand_macro(environment, &expansion, one)? {
                        Ok(Some(new_expansion))
                    } else {
                        Ok(Some(expansion))
                    }
                } else {
                    Ok(Some(expansion))
                }
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

pub fn expand_macro(
    environment: &mut Environment,
    arg: &Expression,
    one: bool,
) -> io::Result<Option<Expression>> {
    let lazy = environment.allow_lazy_fn;
    environment.allow_lazy_fn = false;
    let res = expand_macro_internal(environment, arg, one);
    environment.allow_lazy_fn = lazy;
    res
}

fn expand_macro_all(environment: &mut Environment, arg: &Expression) -> io::Result<Expression> {
    if let Some(exp) = expand_macro(environment, arg, false)? {
        if let Expression::Vector(list, _) = &exp {
            let mut nv = Vec::new();
            for item in &*list.borrow() {
                nv.push(expand_macro_all(environment, &item)?);
            }
            list.replace(nv);
        } else if let Expression::Pair(p, _) = &exp {
            let mut nv = Vec::new();
            for item in exp.iter() {
                nv.push(expand_macro_all(environment, &item)?);
            }
            if let Expression::Pair(np, _) = Expression::cons_from_vec(&mut nv, None) {
                p.replace(np.borrow().clone());
            }
        }
        Ok(exp)
    } else {
        let arg = arg.clone();
        if let Expression::Vector(list, _) = &arg {
            let mut nv = Vec::new();
            for item in &*list.borrow() {
                nv.push(expand_macro_all(environment, &item)?);
            }
            list.replace(nv);
        } else if let Expression::Pair(p, _) = &arg {
            let mut nv = Vec::new();
            for item in arg.iter() {
                nv.push(expand_macro_all(environment, &item)?);
            }
            if let Expression::Pair(np, _) = Expression::cons_from_vec(&mut nv, None) {
                p.replace(np.borrow().clone());
            }
        }
        Ok(arg)
    }
}

fn builtin_expand_macro(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            return if let Some(exp) = expand_macro(environment, arg0, false)? {
                Ok(exp)
            } else {
                Ok(arg0.clone())
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "expand-macro can only have one form (list defining the macro call)",
    ))
}

fn builtin_expand_macro1(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            return if let Some(exp) = expand_macro(environment, arg0, true)? {
                Ok(exp)
            } else {
                Ok(arg0.clone())
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "expand-macro1 can only have one form (list defining the macro call)",
    ))
}

fn builtin_expand_macro_all(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(arg0) = args.next() {
        if args.next().is_none() {
            return expand_macro_all(environment, arg0);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "expand-macro-all can only have one form (list defining the macro call)",
    ))
}

fn builtin_recur(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut arg_list: Vec<Expression> = Vec::new();
    let mut arg_num = 0;
    for a in args {
        let a = eval(environment, a)?;
        arg_list.push(a);
        arg_num += 1;
    }
    environment.state.recur_num_args = Some(arg_num);
    Ok(Expression::with_list(arg_list))
}

fn builtin_gensym(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_some() {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "gensym takes to arguments",
        ))
    } else {
        let gensym_count = &mut environment.state.gensym_count;
        *gensym_count += 1;
        Ok(Expression::Atom(Atom::Symbol(
            environment
                .interner
                .intern(&format!("gs@@{}", *gensym_count)),
        )))
    }
}

fn builtin_jobs(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_some() {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "jobs takes no arguments",
        ))
    } else {
        for (i, job) in environment.jobs.borrow().iter().enumerate() {
            println!(
                "[{}]\t{}\t{:?}\t{:?}",
                i,
                job.status.to_string(),
                job.pids,
                job.names
            );
        }
        Ok(Expression::nil())
    }
}

fn get_stopped_pid(environment: &mut Environment, arg: Option<Expression>) -> Option<u32> {
    if let Some(arg) = arg {
        if let Expression::Atom(Atom::Int(ji)) = arg {
            let ji = ji as usize;
            let jobs = &*environment.jobs.borrow();
            if ji < jobs.len() {
                let pid = jobs[ji].pids[0];
                let mut stop_idx: Option<u32> = None;
                for (i, sp) in environment.stopped_procs.borrow().iter().enumerate() {
                    if *sp == pid {
                        stop_idx = Some(i as u32);
                        break;
                    }
                }
                if let Some(idx) = stop_idx {
                    environment.stopped_procs.borrow_mut().remove(idx as usize);
                }
                Some(pid)
            } else {
                eprintln!("Error job id out of range.");
                None
            }
        } else {
            eprintln!("Error job id must be integer.");
            None
        }
    } else {
        environment.stopped_procs.borrow_mut().pop()
    }
}

fn builtin_bg(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let arg = if let Some(arg) = args.next() {
        if args.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "bg can only have one optional form (job id)",
            ));
        }
        Some(eval(environment, arg)?)
    } else {
        None
    };
    let opid = get_stopped_pid(environment, arg);
    if let Some(pid) = opid {
        let ppid = Pid::from_raw(pid as i32);
        if let Err(err) = signal::kill(ppid, Signal::SIGCONT) {
            eprintln!("Error sending sigcont to wake up process: {}.", err);
        } else {
            mark_job_running(environment, pid);
        }
    }
    Ok(Expression::nil())
}

fn builtin_fg(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let arg = if let Some(arg) = args.next() {
        if args.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "fg can only have one optional form (job id)",
            ));
        }
        Some(eval(environment, arg)?)
    } else {
        None
    };
    let opid = get_stopped_pid(environment, arg);
    if let Some(pid) = opid {
        let term_settings = termios::tcgetattr(nix::libc::STDIN_FILENO).unwrap();
        let ppid = Pid::from_raw(pid as i32);
        if let Err(err) = signal::kill(ppid, Signal::SIGCONT) {
            eprintln!("Error sending sigcont to wake up process: {}.", err);
        } else {
            if let Err(err) = unistd::tcsetpgrp(nix::libc::STDIN_FILENO, ppid) {
                let msg = format!("Error making {} foreground in parent: {}", pid, err);
                eprintln!("{}", msg);
            }
            mark_job_running(environment, pid);
            wait_pid(environment, pid, Some(&term_settings));
        }
    }
    Ok(Expression::nil())
}

fn builtin_version(
    _environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_some() {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "version takes no arguments",
        ))
    } else {
        Ok(Expression::Atom(Atom::StringRef(VERSION_STRING)))
    }
}

fn builtin_command(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let old_form = environment.form_type;
    environment.form_type = FormType::ExternalOnly;
    let mut last_eval = Ok(Expression::nil());
    for a in args {
        last_eval = eval(environment, a);
        if let Err(err) = last_eval {
            environment.form_type = old_form;
            return Err(err);
        }
    }
    environment.form_type = old_form;
    last_eval
}

fn builtin_run_bg(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    environment.run_background = true;
    let mut last_eval = Ok(Expression::nil());
    for a in args {
        last_eval = eval(environment, a);
        if let Err(err) = last_eval {
            environment.run_background = false;
            return Err(err);
        }
    }
    environment.run_background = false;
    last_eval
}

fn builtin_form(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let old_form = environment.form_type;
    environment.form_type = FormType::FormOnly;
    let mut last_eval = Ok(Expression::nil());
    for a in args {
        last_eval = eval(environment, a);
        if let Err(err) = last_eval {
            environment.form_type = old_form;
            return Err(err);
        }
    }
    environment.form_type = old_form;
    last_eval
}

fn builtin_loose_symbols(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let old_loose_syms = environment.loose_symbols;
    environment.loose_symbols = true;
    let mut last_eval = Ok(Expression::nil());
    for a in args {
        last_eval = eval(environment, a);
        if let Err(err) = last_eval {
            environment.loose_symbols = old_loose_syms;
            return Err(err);
        }
    }
    environment.loose_symbols = old_loose_syms;
    last_eval
}

fn builtin_exit(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if let Some(exit_code) = args.next() {
        if args.next().is_none() {
            let exit_code = eval(environment, exit_code)?;
            return if let Expression::Atom(Atom::Int(exit_code)) = exit_code {
                environment.exit_code = Some(exit_code as i32);
                Ok(Expression::nil())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "exit can only take an optional integer (exit code- defaults to 0)",
                ))
            };
        }
    } else {
        environment.exit_code = Some(0);
        return Ok(Expression::nil());
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "exit can only take an optional integer (exit code- defaults to 0)",
    ))
}

fn builtin_error_stack_on(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_none() {
        environment.stack_on_error = true;
        return Ok(Expression::nil());
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "error-stack-on takes no args",
    ))
}

fn builtin_error_stack_off(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_none() {
        environment.stack_on_error = false;
        return Ok(Expression::nil());
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "error-stack-on takes no args",
    ))
}

fn builtin_get_error(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut ret = Expression::nil();
    let old_err = environment.stack_on_error;
    environment.stack_on_error = false;
    for arg in args {
        match eval(environment, &arg) {
            Ok(exp) => ret = exp,
            Err(err) => {
                let mut v = Vec::new();
                v.push(Expression::Atom(Atom::Symbol(
                    environment.interner.intern(":error"),
                )));
                let msg = format!("{}", err);
                v.push(Expression::Atom(Atom::String(msg)));
                environment.stack_on_error = old_err;
                return Ok(Expression::with_list(v));
            }
        }
    }
    environment.stack_on_error = old_err;
    Ok(ret)
}

fn add_usage(doc_str: &mut String, sym: &str, exp: &Expression) {
    let l;
    let p_iter = match exp {
        Expression::Atom(Atom::Lambda(f)) => match &*f.params {
            Expression::Vector(li, _) => {
                l = li.borrow();
                Box::new(l.iter())
            }
            _ => f.params.iter(),
        },
        Expression::Atom(Atom::Macro(m)) => match &*m.params {
            Expression::Vector(li, _) => {
                l = li.borrow();
                Box::new(l.iter())
            }
            _ => m.params.iter(),
        },
        _ => return,
    };
    doc_str.push_str("\n\nUsage: (");
    doc_str.push_str(sym);
    for arg in p_iter {
        if let Expression::Atom(Atom::Symbol(s)) = arg {
            doc_str.push(' ');
            doc_str.push_str(s);
        }
    }
    doc_str.push(')');
}

fn make_doc(exp: &Reference, key: &str) -> io::Result<Expression> {
    let mut new_docs = String::new();
    new_docs.push_str(key);
    new_docs.push_str("\nType: ");
    new_docs.push_str(&exp.exp.display_type());
    if let Some(ns) = &exp.meta.namespace {
        new_docs.push_str("\nNamespace: ");
        new_docs.push_str(&ns);
    }
    if let Some(doc_str) = &exp.meta.doc_string {
        if !doc_str.contains("Usage:") {
            add_usage(&mut new_docs, key, &exp.exp);
        }
        new_docs.push_str("\n\n");
        new_docs.push_str(&doc_str);
    } else {
        add_usage(&mut new_docs, key, &exp.exp);
    }
    new_docs.push('\n');
    Ok(Expression::Atom(Atom::String(new_docs)))
}

fn get_doc(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
    is_raw: bool,
) -> io::Result<Expression> {
    if let Some(key) = args.next() {
        if args.next().is_none() {
            let key = match eval(environment, key)? {
                Expression::Atom(Atom::Symbol(s)) => s,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "doc: first form must evaluate to a symbol",
                    ));
                }
            };
            if key.contains("::") {
                // namespace reference.
                let mut key_i = key.splitn(2, "::");
                if let Some(namespace) = key_i.next() {
                    if let Some(key) = key_i.next() {
                        let namespace = if namespace == "ns" {
                            if let Some(exp) = get_expression(environment, "*ns*") {
                                match &exp.exp {
                                    Expression::Atom(Atom::String(s)) => s.to_string(),
                                    Expression::Atom(Atom::StringRef(s)) => (*s).to_string(),
                                    Expression::Atom(Atom::StringBuf(s)) => s.borrow().to_string(),
                                    _ => "NO_NAME".to_string(),
                                }
                            } else {
                                "NO_NAME".to_string()
                            }
                        } else {
                            namespace.to_string()
                        };
                        if let Some(scope) = get_namespace(environment, &namespace) {
                            if is_raw {
                                if let Some(exp) = scope.borrow().data.get(key) {
                                    if let Some(doc_string) = &exp.meta.doc_string {
                                        return Ok(Expression::Atom(Atom::String(
                                            doc_string.to_string(),
                                        )));
                                    } else {
                                        return Ok(Expression::nil());
                                    }
                                }
                                return Ok(Expression::nil());
                            } else if let Some(exp) = scope.borrow().data.get(key) {
                                return make_doc(&exp, key);
                            }
                        }
                    }
                }
                return Ok(Expression::nil());
            } else if let Some(scope) = get_symbols_scope(environment, &key) {
                if is_raw {
                    if let Some(exp) = scope.borrow().data.get(&key) {
                        if let Some(doc_string) = &exp.meta.doc_string {
                            return Ok(Expression::Atom(Atom::String(doc_string.to_string())));
                        } else {
                            return Ok(Expression::nil());
                        }
                    }
                    return Ok(Expression::nil());
                } else if let Some(exp) = scope.borrow().data.get(&key) {
                    return make_doc(&exp, &key);
                }
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "doc: first form must evaluate to an existing symbol",
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "doc: requires a single symbol to lookup.",
    ))
}

fn builtin_doc(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    get_doc(environment, args, false)
}

fn builtin_doc_raw(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    get_doc(environment, args, true)
}

pub fn builtin_block(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut ret = Expression::nil();
    if let Some(name) = args.next() {
        let name = if let Expression::Atom(Atom::Symbol(n)) = name {
            n
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "block: Name must be a symbol (not evaluated).",
            ));
        };
        for arg in args {
            ret = ret.resolve(environment)?;
            if environment.return_val.is_none() {
                ret = eval_nr(environment, &arg)?;
            }
            let mut returned = false;
            if let Some((ret_name, exp)) = &environment.return_val {
                if let Some(ret_name) = ret_name {
                    if name == ret_name {
                        returned = true;
                        ret = exp.clone();
                    }
                } else {
                    returned = true;
                    ret = exp.clone();
                }
            }
            if returned {
                environment.return_val = None;
                return Ok(ret);
            }
            if environment.return_val.is_some() {
                break;
            }
        }
        Ok(ret)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "block: requires a name.",
        ))
    }
}

pub fn builtin_return_from(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    let mut ret = Expression::nil();
    if let Some(name) = args.next() {
        let name = if let Expression::Atom(Atom::Symbol(n)) = name {
            Some(*n)
        } else if name.is_nil() {
            None
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "return-from: Name should be a symbol or nil (not evaluated).",
            ));
        };
        if let Some(exp) = args.next() {
            if args.next().is_none() {
                ret = eval_nr(environment, exp)?;
            } else {
                return Err(io::Error::new(
        io::ErrorKind::Other,
        "return-from: Requires a block name and optional expression, provided extra form(s).",
                ));
            }
        }
        environment.return_val = Some((name, ret));
        Ok(Expression::nil())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "block: requires a name.",
        ))
    }
}

pub fn builtin_intern_stats(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_some() {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "intern-stats: takes no arguments.",
        ))
    } else {
        println!(
            "allocated bytes: {}\nused bytes: {}\nsymbols interned: {}",
            environment.interner.capacity(),
            environment.interner.used(),
            environment.interner.len()
        );
        Ok(Expression::nil())
    }
}

pub fn builtin_meta_line_no(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_none() {
        if let Some(meta) = &environment.last_meta {
            Ok(Expression::Atom(Atom::Int(meta.line as i64)))
        } else {
            Ok(Expression::nil())
        }
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "meta-line-no: takes no arguments.",
        ))
    }
}

pub fn builtin_meta_column_no(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_none() {
        if let Some(meta) = &environment.last_meta {
            Ok(Expression::Atom(Atom::Int(meta.col as i64)))
        } else {
            Ok(Expression::nil())
        }
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "meta-column-no: takes no arguments.",
        ))
    }
}

pub fn builtin_meta_file_name(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = &Expression>,
) -> io::Result<Expression> {
    if args.next().is_none() {
        if let Some(meta) = &environment.last_meta {
            Ok(Expression::Atom(Atom::StringRef(meta.file)))
        } else {
            Ok(Expression::nil())
        }
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "meta-file-name: takes no arguments.",
        ))
    }
}

macro_rules! ensure_tonicity {
    ($check_fn:expr, $values:expr, $type:ty, $type_two:ty) => {{
        let first = $values.first().ok_or(io::Error::new(
            io::ErrorKind::Other,
            "expected at least one value",
        ))?;
        let rest = &$values[1..];
        fn f(prev: $type, xs: &[$type_two]) -> bool {
            match xs.first() {
                Some(x) => $check_fn(prev, x) && f(x, &xs[1..]),
                None => true,
            }
        };
        if f(first, rest) {
            Ok(Expression::Atom(Atom::True))
        } else {
            Ok(Expression::nil())
        }
    }};
}

macro_rules! ensure_tonicity_all {
    ($check_fn:expr) => {{
        |environment: &mut Environment,
         args: &mut dyn Iterator<Item = &Expression>|
         -> io::Result<Expression> {
            let mut list: Vec<Expression> = Vec::new();
            for arg in args {
                list.push(eval(environment, &arg)?);
            }
            if let Ok(ints) = parse_list_of_ints(environment, &mut list) {
                ensure_tonicity!($check_fn, ints, &i64, i64)
            } else if let Ok(floats) = parse_list_of_floats(environment, &mut list) {
                ensure_tonicity!($check_fn, floats, &f64, f64)
            } else {
                let strings = parse_list_of_strings(environment, &mut list)?;
                ensure_tonicity!($check_fn, strings, &str, String)
            }
        }
    }};
}

pub fn add_builtins<S: BuildHasher>(
    interner: &mut Interner,
    data: &mut HashMap<&'static str, Rc<Reference>, S>,
) {
    let root = interner.intern("root");
    data.insert(
        interner.intern("eval"),
        Rc::new(Expression::make_function(
            builtin_eval,
            "Usage: (eval expression)

Evaluate the provided expression.

If expression is a string read it to make an ast first to evaluate otherwise
evaluate the expression (note eval is a function not a special form, the
provided expression will be evaluated as part of call).

Section: core

Example:
(def 'test-eval-one nil)
(eval \"(set 'test-eval-one \\\"ONE\\\")\")
(test::assert-equal \"ONE\" test-eval-one)
(eval '(set 'test-eval-one \"TWO\"))
(test::assert-equal \"TWO\" test-eval-one)
",
            root,
        )),
    );
    data.insert(
        interner.intern("fncall"),
        Rc::new(Expression::make_function(
            builtin_fncall,
            "Usage: (fncall function arg0 ... argN)

Call the provided function with the supplied arguments.

Section: core

Example:
(def 'test-fncall-one nil)
(fncall set 'test-fncall-one \"ONE\")
(test::assert-equal \"ONE\" test-fncall-one)
(test::assert-equal 10 (fncall + 1 2 7))
",
            root,
        )),
    );
    data.insert(
        interner.intern("apply"),
        Rc::new(Expression::make_function(
            builtin_apply,
            "Usage: (apply function arg* list)

Call the provided function with the suplied arguments, last is a list that will be expanded.

Section: core

Example:
(def 'test-apply-one nil)
(apply set '('test-apply-one \"ONE\"))
(test::assert-equal \"ONE\" test-apply-one)
(test::assert-equal 10 (apply + 1 '(2 7)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("unwind-protect"),
        Rc::new(Expression::make_function(
            builtin_unwind_protect,
            "Usage: (unwind-protect protected cleanup*) -> [protected result]

After evaluation first form, make sure the following cleanup forms run (returns first form's result).

Section: core

Example:
(def 'test-unwind-one nil)
(def 'test-unwind-err (get-error
(unwind-protect (err \"Some protected error\") (set 'test-unwind-one \"got it\"))))
(test::assert-equal '#(:error \"Some protected error\") test-unwind-err)
(test::assert-equal \"got it\" test-unwind-one)

(def 'test-unwind-one nil)
(def 'test-unwind-two nil)
(def 'test-unwind-three nil)
(def 'test-unwind-four nil)
(def 'test-unwind-err (get-error
(unwind-protect
    (progn (set 'test-unwind-one \"set one\")(err \"Some protected error two\")(set 'test-unwind-two \"set two\"))
    (set 'test-unwind-three \"set three\")(set 'test-unwind-four \"set four\"))))
(test::assert-equal '#(:error \"Some protected error two\") test-unwind-err)
(test::assert-equal \"set one\" test-unwind-one)
(test::assert-equal nil test-unwind-two)
(test::assert-equal \"set three\" test-unwind-three)
(test::assert-equal \"set four\" test-unwind-four)
", root
        )),
    );
    data.insert(
        interner.intern("err"),
        Rc::new(Expression::make_function(
            builtin_err,
            "Usage: (err string) -> raises an error

Raise an error with the supplied string.

Section: core

Example:
(def 'test-err-err (get-error (err \"Test Error\")))
(test::assert-equal '#(:error \"Test Error\") test-err-err)
",
            root,
        )),
    );
    data.insert(
        interner.intern("load"),
        Rc::new(Expression::make_function(
            builtin_load,
            "Usage: (load path) -> [last form value]

Read and eval a file (from path- a string).

Section: scripting

Example:
(def 'test-load-one nil)
(def 'test-load-two nil)
(write-line (open \"/tmp/slsh-test-load.testing\" :create :truncate) \"(set 'test-load-one \\\"LOAD TEST\\\") '(1 2 3)\")
(set 'test-load-two (load \"/tmp/slsh-test-load.testing\"))
(test::assert-equal \"LOAD TEST\" test-load-one)
(test::assert-equal '(1 2 3) test-load-two)
", root
        )),
    );
    data.insert(
        interner.intern("length"),
        Rc::new(Expression::make_function(
            builtin_length,
            "Usage: (length expression) -> int

Return length of suplied expression.

Section: core

Example:
(test::assert-equal 0 (length nil))
(test::assert-equal 5 (length \"12345\"))
; Note the unicode symbol is only one char even though it is more then one byte.
(test::assert-equal 6 (length \"12345Σ\"))
(test::assert-equal 3 (length '(1 2 3)))
(test::assert-equal 3 (length '#(1 2 3)))
(test::assert-equal 3 (length (list 1 2 3)))
(test::assert-equal 3 (length (vec 1 2 3)))
(test::assert-equal 1 (length 100))
(test::assert-equal 1 (length 100.0))
(test::assert-equal 1 (length #\\x))
",
            root,
        )),
    );
    data.insert(
        interner.intern("if"),
        Rc::new(Expression::make_special(
            builtin_if,
            "Usage: (if condition then-form else-form?) -> [evaled form result]

If then else conditional.

Section: conditional

Example:
(def 'test-if-one
    (if t \"ONE TRUE\" \"ONE FALSE\"))
(def 'test-if-two
    (if nil \"TWO TRUE\" \"TWO FALSE\"))
(test::assert-equal \"ONE TRUE\" test-if-one)
(test::assert-equal \"TWO FALSE\" test-if-two)

(def 'test-if-one2
    (if t \"ONE2 TRUE\"))
(def 'test-if-two2
    (if nil \"TWO2 TRUE\"))
(test::assert-equal \"ONE2 TRUE\" test-if-one2)
(test::assert-equal nil test-if-two2)
",
            root,
        )),
    );
    data.insert(
        interner.intern("print"),
        Rc::new(Expression::make_function(
            builtin_print,
            "Usage: (print arg0 ... argN) -> nil

Print the arguments (as strings) to *stdout*.

Section: core

Example:
; Use a file for stdout for test.
(dyn '*stdout* (open \"/tmp/sl-sh.print.test\" :create :truncate) (print \"Print test out\"))
(test::assert-equal \"Print test out\" (read-line (open \"/tmp/sl-sh.print.test\" :read)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("println"),
        Rc::new(Expression::make_function(
            builtin_println,
            "Usage: (println arg0 ... argN) -> nil

Print the arguments (as strings) to *stdout* and then a newline.

Section: core

Example:
; Use a file for stdout for test.
(dyn '*stdout* (open \"/tmp/sl-sh.println.test\" :create :truncate) (println \"Println test out\"))
(test::assert-equal \"Println test out\n\" (read-line (open \"/tmp/sl-sh.println.test\" :read)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("eprint"),
        Rc::new(Expression::make_function(
            builtin_eprint,
            "Usage: (eprint arg0 ... argN) -> nil

Print the arguments (as strings) to *stderr*.

Section: core

Example:
; Use a file for stderr for test.
(dyn '*stderr* (open \"/tmp/sl-sh.eprint.test\" :create :truncate) (eprint \"eprint test out\"))
(test::assert-equal \"eprint test out\" (read-line (open \"/tmp/sl-sh.eprint.test\" :read)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("eprintln"),
        Rc::new(Expression::make_function(
            builtin_eprintln,
            "Usage: (eprintln arg0 ... argN) -> nil

Print the arguments (as strings) to *stderr* and then a newline.

Section: core

Example:
; Use a file for stderr for test.
(dyn '*stderr* (open \"/tmp/sl-sh.eprintln.test\" :create :truncate) (eprintln \"eprintln test out\"))
(test::assert-equal \"eprintln test out\n\" (read-line (open \"/tmp/sl-sh.eprintln.test\" :read)))
", root
        )),
    );
    data.insert(
        interner.intern("format"),
        Rc::new(Expression::make_function(
            builtin_format,
            "Usage: (format arg0 ... argN) -> string

Build a formatted string from arguments.

Arguments will be turned into strings.

Section: core

Example:
(test::assert-equal \"stringsome\" (format \"string\" \"some\"))
(test::assert-equal \"string\" (format \"string\" \"\"))
(test::assert-equal \"string 50\" (format \"string\" \" \" 50))
(test::assert-equal \"string 50 100.5\" (format \"string\" \" \" 50 \" \" 100.5))
",
            root,
        )),
    );
    data.insert(
        interner.intern("progn"),
        Rc::new(Expression::make_special(
            builtin_progn,
            "Usage: (progn exp0 ... expN) -> expN

Evaluatate each form and return the last.

Section: core

Example:
(def 'test-progn-one nil)
(def 'test-progn-two nil)
(def 'test-progn-three (progn (set 'test-progn-one \"One\")(set 'test-progn-two \"Two\")\"Three\"))
(test::assert-equal \"One\" test-progn-one)
(test::assert-equal \"Two\" test-progn-two)
(test::assert-equal \"Three\" test-progn-three)
",
            root,
        )),
    );
    data.insert(
        interner.intern("set"),
        Rc::new(Expression::make_function(
            builtin_set,
            "Usage: (set symbol expression) -> expression

Sets an existing expression in the current scope(s).  Return the expression that was set.

Section: core

Example:
(def 'test-progn-one nil)
(def 'test-progn-two nil)
(def 'test-progn-three (progn (set 'test-progn-one \"One\")(set 'test-progn-two \"Two\")\"Three\"))
(test::assert-equal \"One\" test-progn-one)
(test::assert-equal \"Two\" test-progn-two)
(test::assert-equal \"Three\" test-progn-three)
(let ((test-progn-one nil))
    ; set the currently scoped value.
    (test::assert-equal \"1111\" (set 'test-progn-one \"1111\"))
    (test::assert-equal \"1111\" test-progn-one))
; Original outer scope not changed.
(test::assert-equal \"One\" test-progn-one)
",
            root,
        )),
    );
    data.insert(
        interner.intern("export"),
        Rc::new(Expression::make_function(
            builtin_export,
            "Usage: (export symbol string) -> string

Export a key and value to the shell environment.  Second arg will be made a string and returned.

Section: shell

Example:
(test::assert-equal \"ONE\" (export 'TEST_EXPORT_ONE \"ONE\"))
(test::assert-equal \"ONE\" $TEST_EXPORT_ONE)
",
            root,
        )),
    );
    data.insert(
        interner.intern("unexport"),
        Rc::new(Expression::make_function(
            builtin_unexport,
            "Usage: (unexport symbol)

Remove a var from the current shell environment.

Section: shell

Example:
(test::assert-equal \"ONE\" (export 'TEST_EXPORT_ONE \"ONE\"))
(test::assert-equal \"ONE\" $TEST_EXPORT_ONE)
(unexport 'TEST_EXPORT_ONE)
(test::assert-false $TEST_EXPORT_ONE)
",
            root,
        )),
    );
    data.insert(
        interner.intern("def"),
        Rc::new(Expression::make_function(
            builtin_def,
            "Usage: (def symbol expression) -> expression

Adds an expression to the current scope.  Return the expression that was defined.

Section: core

Example:
(def 'test-progn-one nil)
(def 'test-progn-two nil)
(def 'test-progn-three (progn (set 'test-progn-one \"One\")(set 'test-progn-two \"Two\")\"Three\"))
(test::assert-equal \"One\" test-progn-one)
(test::assert-equal \"Two\" test-progn-two)
(test::assert-equal \"Three\" test-progn-three)
(let ((test-progn-one nil))
    ; Add this to tthe let's scope (shadow the outer test-progn-two).
    (test::assert-equal \"Default\" (def 'test-progn-two \"Default\"))
    ; set the currently scoped value.
    (set 'test-progn-one \"1111\")
    (set 'test-progn-two \"2222\")
    (test::assert-equal \"1111\" test-progn-one)
    (test::assert-equal \"2222\" test-progn-two))
; Original outer scope not changed.
(test::assert-equal \"One\" test-progn-one)
",
            root,
        )),
    );
    data.insert(
        interner.intern("undef"),
        Rc::new(Expression::make_function(
            builtin_undef,
            "Usage: (undef symbol)

Remove a symbol from the current scope (if it exists).

Section: core

Example:
(def 'test-undef nil)
(test::assert-true (def? 'test-undef))
(undef 'test-undef)
(test::assert-false (def? 'test-undef))
",
            root,
        )),
    );
    data.insert(
        interner.intern("dyn"),
        Rc::new(Expression::make_function(
            builtin_dyn,
            "Usage: (dyn key value expression) -> nil

Creates a dynamic binding for key, assigns value to it and evals expression under it.

The binding is gone once the dyn form ends. The binding will take precedence over
any other binding in any scope with that name for any form that evaluates as a
result of the dynamic binding (for instance creating a dynamic binding for
*stdout* will cause all output to stdout to use the new binding in any print's
used indirectly).

Section: core

Example:
(defn test-dyn-fn () (print \"Print dyn out\"))
(dyn '*stdout* (open \"/tmp/sl-sh.dyn.test\" :create :truncate) (test-dyn-fn))
(test::assert-equal \"Print dyn out\" (read-line (open \"/tmp/sl-sh.dyn.test\" :read)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("to-symbol"),
        Rc::new(Expression::make_function(
            builtin_to_symbol,
            "Usage: (to-symbol expression) -> symbol

Convert a string, symbol, int or float to a symbol.

If the symbol is new it will be interned.

Section: core

Example:
(def 'test-to-symbol-sym nil)
(test::assert-true (symbol? (to-symbol 55)))
(test::assert-true (symbol? (to-symbol 55.0)))
(test::assert-true (symbol? (to-symbol \"to-symbol-test-new-symbol\")))
(test::assert-true (symbol? (to-symbol (str-buf \"to-symbol-test-new-symbol-buf\"))))
(test::assert-true (symbol? (to-symbol 'test-to-symbol-sym)))
(test::assert-true (symbol? (to-symbol (symbol-name 'test-to-symbol-sym))))
",
            root,
        )),
    );
    data.insert(
        interner.intern("symbol-name"),
        Rc::new(Expression::make_function(
            builtin_symbol_name,
            "Usage: (symbol-name symbol) -> string

Convert a symbol to its string representation.

The string will be the symbol name as a string.

Section: core

Example:
(def 'test-symbol-name-sym nil)
(test::assert-true (string? (symbol-name 'test-symbol-name-sym)))
(test::assert-equal \"test-symbol-name-sym\" (symbol-name 'test-symbol-name-sym))
",
            root,
        )),
    );
    data.insert(
        interner.intern("fn"),
        Rc::new(Expression::make_special(
            builtin_fn,
            "Usage: (fn (x) (x + 1))

Create a function (lambda).

Section: core
",
            root,
        )),
    );
    data.insert(
        interner.intern("quote"),
        Rc::new(Expression::make_special(
            builtin_quote,
            "Usage: (quote expression) -> expression

Return expression without evaluation.  The reader macro ' will expand to a quote form.

Section: core

Example:
(test::assert-equal (list 1 2 3) (quote (1 2 3)))
(test::assert-equal (list 1 2 3) '(1 2 3))
(test::assert-equal '(1 2 3) (quote (1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("bquote"),
        Rc::new(Expression::make_special(
            builtin_bquote,
            "Usage: (bquote expression) -> expression

Return expression without evaluation.  The reader macro ` will expand to a bquote form.

The bquote form (unlike quote) allows for symbol/form evaluation using , or ,@.

Section: core

Example:
(test::assert-equal (list 1 2 3) (bquote (1 2 3)))
(test::assert-equal (list 1 2 3) `(1 2 3))
(test::assert-equal `(1 2 3) (bquote (1 2 3)))
(test::assert-equal `(1 2 3) '(1 2 3))
(def 'test-bquote-one 1)
(def 'test-bquote-list '(1 2 3))
(test::assert-equal (list 1 2 3) (bquote (,test-bquote-one 2 3)))
(test::assert-equal (list 1 2 3) (bquote (,@test-bquote-list)))
",
            root,
        )),
    );
    /*data.insert(
        "spawn"),
        Rc::new(Expression::Func(builtin_spawn)),
    );*/
    data.insert(
        interner.intern("and"),
        Rc::new(Expression::make_special(builtin_and,
        "Usage: (and exp0 ... expN) -> [nil or expN result]

Evaluates each form until one produces nil (false), produces nil if any form is nil or the result of the last expression.

The and form will stop evaluating when the first expression produces nil.

Section: conditional

Example:
(test::assert-false (and nil (err \"and- can not happen\")))
(test::assert-equal \"and- done\" (and t \"and- done\"))
(test::assert-equal \"and- done\" (and t t \"and- done\"))
(test::assert-equal 6 (and t t (+ 1 2 3)))
(test::assert-equal 6 (and (/ 10 5) (* 5 2) (+ 1 2 3)))
", root)),
    );
    data.insert(
        interner.intern("or"),
        Rc::new(Expression::make_special(
            builtin_or,
            "Usage: (or exp0 ... expN) -> [nil or first non nil expression]

Evaluates each form until one produces a non-nil result, produces nil if all expressions are nil.

The or form will stop evaluating when the first expression produces non-nil.

Section: conditional

Example:
(test::assert-true (or nil nil t (err \"and- can not happen\")))
(test::assert-false (or nil nil nil))
(test::assert-equal \"or- done\" (or nil \"or- done\"))
(test::assert-equal \"or- done\" (or nil nil \"or- done\"))
(test::assert-equal 6 (or nil nil (+ 1 2 3)))
(test::assert-equal 2 (or (/ 10 5) (* 5 2) (+ 1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("not"),
        Rc::new(Expression::make_function(
            builtin_not,
            "Usage: (not expression)

Return true if expression is nil.

Section: conditional

Example:
(test::assert-true (not nil))
(test::assert-false (not 10))
(test::assert-false (not t))
(test::assert-false (not (+ 1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("null"),
        Rc::new(Expression::make_function(
            builtin_not,
            "Usage: (null expression)

Return true if expression is nil (null).

Section: conditional

Example:
(test::assert-true (null nil))
(test::assert-false (null 10))
(test::assert-false (null t))
(test::assert-false (null (+ 1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("def?"),
        Rc::new(Expression::make_function(
            builtin_is_def,
            "Usage: (def? expression) -> t|nil

Return true if symbol is defined.

Expression will be evaluated and if a symbol or string it will look up that
name in the symbol table and return true if it exists.

Section: core

Example:
(def 'test-is-def t)
(test::assert-true (def? 'test-is-def))
(test::assert-true (def? \"test-is-def\"))
(test::assert-true (def? (symbol-name 'test-is-def)))
(test::assert-false (def? 'test-is-def-not-defined))
(test::assert-false (def? \"test-is-def-not-defined\"))
",
            root,
        )),
    );
    data.insert(
        interner.intern("macro"),
        Rc::new(Expression::make_function(
            builtin_macro,
            "Usage: (macro (&rest args) `(apply + ,@args))

Define an anonymous macro.

Section: core
",
            root,
        )),
    );
    data.insert(
        interner.intern("expand-macro"),
        Rc::new(Expression::make_special(
            builtin_expand_macro,
            "Usage: (expand-macro expression)

Expands a macro expression.  If that expansion is also a macro then expand it recursively.

Just returns the expression if not a macro.

Section: core

Example:
(test::assert-equal '(apply def 'xx '#(\"value\")) (expand-macro (defq xx \"value\")))
(test::assert-equal '(
    (fn
        #(i)
        (progn
            (if
                (> (length '(1 2 3)) 0)
                (core::loop
                    (plist)
                    ('(1 2 3))
                    (progn
                        (core::setq i (core::first plist)) nil
                        (if
                            (> (length plist) 1)
                            (recur (core::rest plist)))))))) nil)
    (expand-macro (for i '(1 2 3) ())))
(test::assert-equal '(1 2 3) (expand-macro (1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("expand-macro1"),
        Rc::new(Expression::make_special(
            builtin_expand_macro1,
            "Usage: (expand-macro1 expression)

Expands a macro expression.  Only expand the first macro.

Just returns the expression if not a macro.

Section: core

Example:
(test::assert-equal '(apply def 'xx '#(\"value\")) (expand-macro1 (defq xx \"value\")))
(test::assert-equal '(core::let
    ((i))
    (if
        (> (length '(1 2 3)) 0)
        (core::loop
            (plist)
            ('(1 2 3))
            (progn
                (core::setq i (core::first plist)) nil
                (if
                    (> (length plist) 1)
                    (recur (core::rest plist)))))))
    (expand-macro1 (for i '(1 2 3) ())))
(test::assert-equal '(1 2 3) (expand-macro1 (1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("expand-macro-all"),
        Rc::new(Expression::make_special(
            builtin_expand_macro_all,
            "Usage: (expand-macro-all expression)

Expands a macro expression like expand-macro but also expand any embedded macros.  

Just returns the expression if not a macro.

Section: core

Example:
(test::assert-equal '(apply def 'xx '#(\"value\")) (expand-macro-all (defq xx \"value\")))
(test::assert-equal '(
    (fn
        #(i)
        (progn
            (if
                (> (length '(1 2 3)) 0)
                (
                    (fn
                        (plist)
                        (progn
                            (apply set 'i '#((core::first plist))) nil
                            (if
                                (> (length plist) 1)
                                (recur (core::rest plist)))))
                    '(1 2 3))))) nil)
    (expand-macro-all (for i '(1 2 3) ())))
(test::assert-equal '(1 2 3) (expand-macro-all (1 2 3)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("recur"),
        Rc::new(Expression::make_function(builtin_recur, "Usage: (recur &rest)

Section: core", root)),
    );
    data.insert(
        interner.intern("gensym"),
        Rc::new(Expression::make_function(
            builtin_gensym,
            "Usage: (gensym) -> symbol

Generate a unique symbol.

Gensym uses a prefix of gs@@ followed by an incrementing counter.
It is useful to generate unique symbol names when writing macros (for instance).

Section: core

Example:
(def 'test-gensym-one (gensym))
(def 'test-gensym-two (gensym))
(def 'test-gensym-three (gensym))
(test::assert-equal \"gs@@1\" (symbol-name test-gensym-one))
(test::assert-equal \"gs@@2\" (symbol-name test-gensym-two))
(test::assert-equal \"gs@@3\" (symbol-name test-gensym-three))
(test::assert-true (symbol? (gensym)))
(test::assert-true (symbol? test-gensym-one))
(test::assert-true (symbol? test-gensym-two))
(test::assert-true (symbol? test-gensym-three))
",
            root,
        )),
    );
    data.insert(
        interner.intern("jobs"),
        Rc::new(Expression::make_function(
            builtin_jobs,
            "Usage: (jobs)

Print list of jobs with ids.

Section: shell

Example:
;(jobs)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("bg"),
        Rc::new(Expression::make_function(
            builtin_bg,
            "Usage: (bg job-id?)

Put a job in the background.

If no job id is specified use the last job.

Section: shell

Example:
;(bg)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("fg"),
        Rc::new(Expression::make_function(
            builtin_fg,
            "Usage: (fg job-id?)

Put a job in the foreground.

If no job id is specified use the last job.

Section: shell

Example:
;(fg)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("version"),
        Rc::new(Expression::make_function(
            builtin_version,
            "Usage: (version)

Produce executable version as string.

Section: shell

Example:
(test::assert-true (string? (version)))
",
            root,
        )),
    );
    data.insert(
        interner.intern("command"),
        Rc::new(Expression::make_special(
            builtin_command,
            "Usage: (command exp0 ... expN)

Only execute system commands not forms within this form.

Section: shell

Example:
(test::assert-equal '#(:error \"Failed to execute [str string]: No such file or directory (os error 2)\") (get-error (command (str \"string\"))))
(test::assert-equal \"Some String\n\" (str (command (echo \"Some String\"))))
", root
        )),
    );
    data.insert(
        interner.intern("run-bg"),
        Rc::new(Expression::make_special(
            builtin_run_bg,
            "Usage: (run-bg exp0 ... expN)

Like progn except any system commands started within form will be in the background.

Section: shell

Example:
;(run-bg gitk)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("form"),
        Rc::new(Expression::make_special(
            builtin_form,
            "Usage: (form exp0 ... expN)

Like progn but do not execute system commands within this form.

Section: shell

Example:
(test::assert-equal '#(:error \"Not a valid form true, not found.\") (get-error (form (true))))
(test::assert-equal \"Some String\" (form (str \"Some String\")))
",
            root,
        )),
    );
    data.insert(
        interner.intern("loose-symbols"),
        Rc::new(Expression::make_special(
            builtin_loose_symbols,
            "Usage: (loose-symbols exp0 ... expN)

Within this form any undefined symbols become strings.

Section: shell

Example:
(test::assert-equal \"Some_Result\" (loose-symbols Some_Result))
",
            root,
        )),
    );
    data.insert(
        interner.intern("exit"),
        Rc::new(Expression::make_function(
            builtin_exit,
            "Usage: (exit code?)

Exit shell with optional status code.

Section: shell

Example:
;(exit)
;(exit 0)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("error-stack-on"),
        Rc::new(Expression::make_function(
            builtin_error_stack_on,
            "Usage: (error-stack-on)

Print the eval stack on error.

Section: core

Example:
;(error-stack-on)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("error-stack-off"),
        Rc::new(Expression::make_function(
            builtin_error_stack_off,
            "Usage: (error-stack-off)

Do not print the eval stack on error.

Section: core

Example:
;(error-stack-off)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("get-error"),
        Rc::new(Expression::make_function(
            builtin_get_error,
            "Usage: (get-error exp0 ... expN)

Evaluate each form (like progn) but on error return #(:error msg) instead of aborting.

If there is no error will return the value of the last expression.

Section: core

Example:
(test::assert-equal '#(:error \"Some Error\") (get-error (err \"Some Error\")))
(test::assert-equal \"Some String\" (get-error \"Some String\"))
(test::assert-equal \"Some Other String\" (get-error (def 'test-get-error \"Some \") (str test-get-error \"Other String\")))
", root
        )),
    );
    data.insert(
        interner.intern("doc"),
        Rc::new(Expression::make_function(
            builtin_doc,
            "Usage: (doc symbol)

Return the doc string for a symbol or nil if no string.

Section: core

Example:
;(doc 'car)
t
",
            root,
        )),
    );
    data.insert(
        interner.intern("doc-raw"),
        Rc::new(Expression::make_function(
            builtin_doc_raw,
            "Usage: (doc-raw symbol)

Return the raw (unexpanded) doc string for a symbol or nil if no string.

Section: core

Example:
;(doc-raw 'car)
t
",
            root,
        )),
    );

    data.insert(
        interner.intern("block"),
        Rc::new(Expression::make_special(
            builtin_block,
            "Usage: (block name form*)

Create a block with name (name is not evaluated), if no [return-from](#root::return-from) encountered then
return last expression (like progn).

Section: core

Example:
(test::assert-equal '(4 5) (block xxx '(1 2) (return-from xxx '(4 5)) '(a b) '(2 3)))
(test::assert-equal '(4 5) (block xxx '(1 2) (return-from nil '(4 5)) '(a b) '(2 3)))
(test::assert-equal '(5 6) (block xxx '(1 2) (block yyy (return-from xxx '(5 6)) '(a b)) '(2 3)))
(test::assert-equal '(5 6) (block xxx '(1 2) (block yyy ((fn (p) (return-from xxx p)) '(5 6)) '(a b)) '(2 3)))
(test::assert-equal '(2 3) (block xxx '(1 2) (block yyy (return-from yyy t) '(a b)) '(2 3)))
", root
        )),
    );

    data.insert(
        interner.intern("return-from"),
        Rc::new(Expression::make_special(
            builtin_return_from,
            "Usage: (return-from name expression?)

Causes enclosing [block](#root::block) with name (name is not evaluated) to evaluate to expression.

Section: core

Example:
(test::assert-equal '(4 5) (block xxx '(1 2) (return-from xxx '(4 5)) '(a b) '(2 3)))
(test::assert-equal '(4 5) (block xxx '(1 2) (return-from nil '(4 5)) '(a b) '(2 3)))
(test::assert-equal '(5 6) (block xxx '(1 2) (block yyy (return-from xxx '(5 6)) '(a b)) '(2 3)))
(test::assert-equal '(5 6) (block xxx '(1 2) (block yyy ((fn (p) (return-from xxx p)) '(5 6)) '(a b)) '(2 3)))
(test::assert-equal '(2 3) (block xxx '(1 2) (block yyy (return-from yyy t) '(a b)) '(2 3)))
", root
        )),
    );

    data.insert(
        interner.intern("intern-stats"),
        Rc::new(Expression::make_special(
            builtin_intern_stats,
            "Usage: (intern-stats)

Prints the stats for interned symbols.

Section: core

Example:
;(intern-stats)
t
",
            root,
        )),
    );

    data.insert(
        interner.intern("meta-line-no"),
        Rc::new(Expression::make_special(
            builtin_meta_line_no,
            "Usage: (meta-line-no)

Line number from the file this came from.

Section: core

Example:
;(meta-line-no)
t
",
            root,
        )),
    );

    data.insert(
        interner.intern("meta-column-no"),
        Rc::new(Expression::make_special(
            builtin_meta_column_no,
            "Usage: (meta-column-no)

Column number from the file this came from.

Section: core

Example:
;(meta-column-no)
t
",
            root,
        )),
    );

    data.insert(
        interner.intern("meta-file-name"),
        Rc::new(Expression::make_special(
            builtin_meta_file_name,
            "Usage: (meta-file-name)

File name of the file this came from.

Section: core

Example:
;(meta-file-name)
t
",
            root,
        )),
    );

    data.insert(
        interner.intern("="),
        Rc::new(Expression::make_function(
            |environment: &mut Environment,
             parts: &mut dyn Iterator<Item = &Expression>|
             -> io::Result<Expression> {
                let mut args: Vec<Expression> = Vec::new();
                for a in parts {
                    args.push(eval(environment, &a)?);
                }
                if let Ok(ints) = parse_list_of_ints(environment, &mut args) {
                    ensure_tonicity!(|a, b| a == b, ints, &i64, i64)
                } else if let Ok(floats) = parse_list_of_floats(environment, &mut args) {
                    ensure_tonicity!(|a, b| ((a - b) as f64).abs() < 0.000_001, floats, &f64, f64)
                } else {
                    let strings = parse_list_of_strings(environment, &mut args)?;
                    ensure_tonicity!(|a, b| a == b, strings, &str, String)
                }
            },
            "Usage: (= val0 ... valN)

Equals.  Works for int, float or string.

Section: conditional

Example:
(test::assert-false (= 1 2))
(test::assert-true (= 2 2))
(test::assert-true (= 2 2 2))
(test::assert-false (= 3 2 2))
(test::assert-false (= 3.0 2.0))
(test::assert-true (= 2.0 2.0))
(test::assert-true (= 2.0 2.0 2.0))
(test::assert-false (= 3.0 2.0 2.0))
(test::assert-false (= 2.1 2.0 3.0))
(test::assert-false (= 2 1))
(test::assert-false (= 3 2 1))
(test::assert-false (= 1.1 1.0))
(test::assert-true (= 1.1 1.1))
(test::assert-false (= 3 2 3))
(test::assert-false (= \"aab\" \"aaa\"))
(test::assert-true (= \"aaa\" \"aaa\"))
(test::assert-true (= \"aaa\" \"aaa\" \"aaa\"))
(test::assert-false (= \"aaa\" \"aaaa\" \"aaa\"))
(test::assert-false (= \"ccc\" \"aab\" \"aaa\"))
(test::assert-false (= \"aaa\" \"aab\"))
",
            root,
        )),
    );
    data.insert(
        interner.intern(">"),
        Rc::new(Expression::make_function(
            ensure_tonicity_all!(|a, b| a > b),
            "Usage: (> val0 ... valN)

Greater than.  Works for int, float or string.

Section: conditional

Example:
(test::assert-false (> 1 2))
(test::assert-false (> 2 2))
(test::assert-false (> 2 2 2))
(test::assert-false (> 3 2 2))
(test::assert-true (> 3.0 2.0))
(test::assert-false (> 2.0 2.0))
(test::assert-false (> 2.0 2.0 2.0))
(test::assert-false (> 3.0 2.0 2.0))
(test::assert-false (> 2.1 2.0 3.0))
(test::assert-true (> 2 1))
(test::assert-true (> 3 2 1))
(test::assert-true (> 1.1 1.0))
(test::assert-false (> 3 2 3))
(test::assert-true (> \"aab\" \"aaa\"))
(test::assert-false (> \"aaa\" \"aaa\"))
(test::assert-true (> \"ccc\" \"aab\" \"aaa\"))
(test::assert-false (> \"aaa\" \"aab\"))
",
            root,
        )),
    );
    data.insert(
        interner.intern(">="),
        Rc::new(Expression::make_function(
            ensure_tonicity_all!(|a, b| a >= b),
            "Usage: (>= val0 ... valN)

Greater than or equal.  Works for int, float or string.

Section: conditional

Example:
(test::assert-false (>= 1 2))
(test::assert-true (>= 2 2))
(test::assert-true (>= 2 2 2))
(test::assert-true (>= 3 2 2))
(test::assert-true (>= 3.0 2.0))
(test::assert-true (>= 2.0 2.0))
(test::assert-true (>= 2.0 2.0 2.0))
(test::assert-true (>= 3.0 2.0 2.0))
(test::assert-false (>= 2.1 2.0 3.0))
(test::assert-true (>= 2 1))
(test::assert-true (>= 1.1 1.0))
(test::assert-false (>= 3 2 3))
(test::assert-true (>= \"aab\" \"aaa\"))
(test::assert-true (>= \"aaa\" \"aaa\"))
(test::assert-true (>= \"ccc\" \"aab\" \"aaa\"))
(test::assert-false (>= \"aaa\" \"aab\"))
",
            root,
        )),
    );
    data.insert(
        interner.intern("<"),
        Rc::new(Expression::make_function(
            ensure_tonicity_all!(|a, b| a < b),
            "Usage: (< val0 ... valN)

Less than.  Works for int, float or string.

Section: conditional

Example:
(test::assert-true (< 1 2))
(test::assert-true (< 1 2 3 4))
(test::assert-false (< 2 2))
(test::assert-false (< 2 2 2))
(test::assert-false (< 2 2 3))
(test::assert-true (< 1.0 2.0))
(test::assert-false (< 2.0 2.0))
(test::assert-false (< 2.0 2.0 2.0))
(test::assert-false (< 2.0 2.0 3.0))
(test::assert-false (< 2.1 2.0 3.0))
(test::assert-false (< 2 1))
(test::assert-false (< 3 2 3))
(test::assert-true (< \"aaa\" \"aab\"))
(test::assert-false (< \"aaa\" \"aaa\"))
(test::assert-true (< \"aaa\" \"aab\" \"ccc\"))
(test::assert-false (< \"baa\" \"aab\"))
",
            root,
        )),
    );
    data.insert(
        interner.intern("<="),
        Rc::new(Expression::make_function(
            ensure_tonicity_all!(|a, b| a <= b),
            "Usage: (<= val0 ... valN)

Less than or equal.  Works for int, float or string.

Section: conditional

Example:
(test::assert-true (<= 1 2))
(test::assert-true (<= 2 2))
(test::assert-true (<= 2 2 2))
(test::assert-true (<= 2 2 3))
(test::assert-true (<= 1.0 2.0))
(test::assert-true (<= 2.0 2.0))
(test::assert-true (<= 2.0 2.0 2.0))
(test::assert-true (<= 2.0 2.0 3.0))
(test::assert-false (<= 2.1 2.0 3.0))
(test::assert-false (<= 2 1))
(test::assert-false (<= 3 2 3))
(test::assert-true (<= \"aaa\" \"aab\"))
(test::assert-true (<= \"aaa\" \"aaa\"))
(test::assert-true (<= \"aaa\" \"aab\" \"ccc\"))
(test::assert-false (<= \"baa\" \"aab\"))
",
            root,
        )),
    );
}
