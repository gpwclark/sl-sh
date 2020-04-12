use glob::{glob, glob_with, MatchOptions};
use liner::{Completer, CursorPosition, Event, EventKind};
use std::cell::RefCell;
use std::env;
use std::path::Path;
use std::rc::Rc;

use crate::builtins_util::compress_tilde;
use crate::builtins_util::expand_tilde;
use crate::environment::*;
use crate::eval::*;
use crate::types::*;

/// Unescape filenames for the completer so that special characters will be properly shown.
fn unescape(input: &str) -> String {
    let mut output = Vec::with_capacity(input.len());
    let mut check = false;
    for character in input.bytes() {
        match character {
            b'\\' if !check => check = true,
            b'(' | b')' | b'"' | b'\'' | b' ' if check => {
                output.push(character);
                check = false;
            }
            _ if check => {
                output.extend(&[b'\\', character]);
                check = false;
            }
            _ => output.push(character),
        }
    }
    unsafe { String::from_utf8_unchecked(output) }
}

/// Escapes filenames from the completer so that special characters will be properly escaped.
/// If collapse_tilde is true also replace home dir paths with ~.
fn escape(input: &str, collapse_tilde: bool) -> String {
    let tinput;
    let input = if collapse_tilde {
        match compress_tilde(input) {
            Some(s) => {
                tinput = s;
                &tinput
            }
            None => input,
        }
    } else {
        input
    };
    let mut output = Vec::with_capacity(input.len());
    for character in input.bytes() {
        match character {
            b'(' | b')' | b'"' | b'\'' | b' ' => output.push(b'\\'),
            _ => (),
        }
        output.push(character);
    }
    unsafe { String::from_utf8_unchecked(output) }
}

#[derive(Debug)]
enum CompType {
    Nothing,
    Command,
    CommandParen,
    EnvVar,
    Symbols,
    Other,
}

enum HookResult {
    Default,
    Path,
    UseList(Vec<String>),
}

pub struct ShellCompleter {
    environment: Rc<RefCell<Environment>>,
    comp_type: CompType,
    args: Vec<String>,
}

impl ShellCompleter {
    pub fn new(environment: Rc<RefCell<Environment>>) -> ShellCompleter {
        ShellCompleter {
            environment,
            comp_type: CompType::Nothing,
            args: Vec::new(),
        }
    }

    fn run_hook(&mut self) -> HookResult {
        if self.args.is_empty() {
            return HookResult::Default;
        }
        let comp_exp = get_expression(&self.environment.borrow(), "__completion_hook");
        if let Some(comp_exp) = comp_exp {
            let exp = match comp_exp.exp.get() {
                ExpEnum::Atom(Atom::Lambda(_)) => {
                    let mut v = Vec::with_capacity(1 + self.args.len());
                    let mut environment = self.environment.borrow_mut();
                    let data = ExpEnum::Atom(Atom::Symbol(
                            environment.interner.intern("__completion_hook"),
                        ));
                    v.push(Expression::alloc_data(
                        &mut environment.gc,
                        data,
                    ));
                    for a in self.args.drain(..) {
                        v.push(Expression::alloc_data(
                            &mut environment.gc,
                            ExpEnum::Atom(Atom::String(a)),
                        ));
                    }
                    Expression::with_list(&mut environment.gc, v)
                }
                _ => {
                    eprintln!("WARNING: __completion_hook not a function, ignoring.");
                    return HookResult::Default;
                }
            };
            let envir = &mut self.environment.borrow_mut();
            match eval(envir, &exp) {
                Ok(res) => {
                    match res.get_mut() {
                        ExpEnum::Atom(Atom::StringRef(s)) | ExpEnum::Atom(Atom::Symbol(s)) => {
                            match *s {
                                "path" => HookResult::Path,
                                "default" => HookResult::Default,
                                _ => {
                                    eprintln!("ERROR: unknown completion hook command, {}", s);
                                    HookResult::Default
                                }
                            }
                        }
                        ExpEnum::Atom(Atom::String(s)) => match s.as_ref() {
                            "path" => HookResult::Path,
                            "default" => HookResult::Default,
                            _ => {
                                eprintln!("ERROR: unknown completion hook command, {}", s);
                                HookResult::Default
                            }
                        },
                        ExpEnum::Atom(Atom::StringBuf(s)) => match s.borrow().as_ref() {
                            "path" => HookResult::Path,
                            "default" => HookResult::Default,
                            _ => {
                                eprintln!("ERROR: unknown completion hook command, {}", s.borrow());
                                HookResult::Default
                            }
                        },
                        ExpEnum::Vector(list) => {
                            let mut v = Vec::with_capacity(list.len());
                            for l in list.drain(..) {
                                let s = match l.as_string(envir) {
                                    Ok(s) => s.trim().to_string(),
                                    Err(_) => "ERROR".to_string(),
                                };
                                v.push(s);
                            }
                            HookResult::UseList(v)
                        }
                        ExpEnum::Pair(_, _) => {
                            let mut v = Vec::new();
                            for l in res.iter() {
                                let s = match l.as_string(envir) {
                                    Ok(s) => s.trim().to_string(),
                                    Err(_) => "ERROR".to_string(),
                                };
                                v.push(s);
                            }
                            HookResult::UseList(v)
                        }
                        ExpEnum::Nil => HookResult::Default,
                        _ => {
                            eprintln!("WARNING: unexpected result from __completion_hook, {:?}, ignoring.", res);
                            HookResult::Default
                        }
                    }
                }
                Err(err) => {
                    eprintln!("ERROR calling __completion_hook: {}", err);
                    HookResult::Default
                }
            }
        } else {
            HookResult::Default
        }
    }
}

impl Completer for ShellCompleter {
    fn completions(&mut self, start: &str) -> Vec<String> {
        match self.comp_type {
            CompType::Nothing => Vec::new(),
            CompType::Command => {
                let mut ret = get_dir_matches(start);
                find_lisp_fns(&self.environment.borrow(), &mut ret, start);
                find_exes(&mut ret, start);
                ret
            }
            CompType::CommandParen => {
                let mut ret: Vec<String> = Vec::new();
                find_lisp_fns(&self.environment.borrow(), &mut ret, start);
                find_exes(&mut ret, start);
                ret
            }
            CompType::EnvVar => match self.run_hook() {
                HookResult::Default => get_env_matches(start),
                HookResult::Path => get_path_matches(start),
                HookResult::UseList(list) => list,
            },
            CompType::Symbols => match self.run_hook() {
                HookResult::Default => {
                    let mut ret: Vec<String> = Vec::new();
                    find_lisp_symbols(&self.environment.borrow(), &mut ret, start);
                    ret
                }
                HookResult::Path => get_path_matches(start),
                HookResult::UseList(list) => list,
            },
            CompType::Other => match self.run_hook() {
                HookResult::Default => {
                    let mut ret = get_dir_matches(start);
                    find_lisp_symbols(&self.environment.borrow(), &mut ret, start);
                    ret
                }
                HookResult::Path => get_path_matches(start),
                HookResult::UseList(list) => list,
            },
        }
    }

    fn on_event(&mut self, event: Event<'_, '_>) {
        self.args.clear();
        if let EventKind::BeforeComplete = event.kind {
            let (words, pos) = event.editor.get_words_and_cursor_position();
            for word_limits in &words {
                let word = event
                    .editor
                    .current_buffer()
                    .range(word_limits.0, word_limits.1);
                self.args.push(word);
            }
            if String::from(event.editor.current_buffer().clone()).ends_with(' ') {
                self.args.push("".to_string());
            }
            self.comp_type = match pos {
                _ if words.is_empty() => CompType::Nothing,
                CursorPosition::InWord(0) => CompType::Command,
                CursorPosition::OnWordRightEdge(index) => {
                    if index == 0 {
                        CompType::Command
                    } else {
                        let word_limits = words.get(index - 1);
                        let is_form_start = word_limits
                            .map(|(start, end)| event.editor.current_buffer().range(*start, *end))
                            .filter(|filename| filename == "(")
                            .is_some();
                        if is_form_start {
                            CompType::CommandParen
                        } else {
                            let word_limits = words.into_iter().nth(index);
                            let is_env_var = word_limits
                                .map(|(start, end)| event.editor.current_buffer().range(start, end))
                                .filter(|filename| filename.starts_with('$'))
                                .is_some();
                            if is_env_var {
                                CompType::EnvVar
                            } else {
                                let is_symbol = word_limits
                                    .map(|(start, end)| {
                                        event.editor.current_buffer().range(start, end)
                                    })
                                    .filter(|filename| filename.starts_with('\''))
                                    .is_some();
                                if is_symbol {
                                    CompType::Symbols
                                } else {
                                    CompType::Other
                                }
                            }
                        }
                    }
                }
                _ => CompType::Other,
            };
        }
    }
}

fn find_file_completions(org_start: &str, cur_path: &Path) -> Vec<String> {
    let mut res = Vec::new();
    let mut tilde_expanded = false;
    let tinput;
    let org_start = match expand_tilde(org_start) {
        Some(s) => {
            tilde_expanded = true;
            tinput = s;
            &tinput
        }
        None => org_start,
    };

    let (start, need_quotes) = if org_start.starts_with('"') {
        (&org_start[1..], true)
    } else {
        (org_start, false)
    };
    let unescaped = unescape(start);
    let mut split_start = unescaped.split('/');
    let mut pat = String::new();
    let mut using_cur_path = false;
    if start.starts_with('/') {
        split_start.next();
        pat.push('/');
    } else {
        using_cur_path = true;
        pat.push_str(&cur_path.to_string_lossy());
        pat.push('/');
    }
    for element in split_start {
        if !element.is_empty() {
            pat.push_str(element);
            let path = Path::new(&pat);
            if element != "." && element != ".." && !path.exists() {
                pat.push('*');
            }
            pat.push('/');
        } else {
            pat.push_str("*");
            pat.push('/');
        }
    }

    pat.pop(); // pop out the last '/' character
    if !pat.ends_with('*') {
        pat.push('*')
    }
    let cur_path_str = cur_path.to_string_lossy().to_string();
    let globs = glob_with(
        &pat,
        MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        },
    );
    match globs {
        Ok(paths) => {
            for p in paths {
                match p {
                    Ok(p) => {
                        let p_lossy = p.to_string_lossy();
                        let need_slash = p.is_dir() && !p_lossy.ends_with('/');
                        let mut item = if using_cur_path
                            && p_lossy.starts_with(&cur_path_str)
                            && p_lossy.len() > cur_path_str.len()
                        {
                            p_lossy[(cur_path_str.len() + 1)..].to_string()
                        } else {
                            p_lossy.to_string()
                        };
                        if need_slash {
                            item.push('/');
                        }
                        let val = if need_quotes {
                            format!("\"{}", item)
                        } else {
                            item.to_string()
                        };
                        res.push(escape(&val, tilde_expanded));
                    }
                    Err(_err) => {}
                }
            }
        }
        Err(_err) => {}
    }
    res
}

fn get_dir_matches(start: &str) -> Vec<String> {
    match env::current_dir() {
        Ok(p) => find_file_completions(start, &p),
        Err(_err) => Vec::new(),
    }
}

fn get_path_matches(start: &str) -> Vec<String> {
    let mut res = get_dir_matches(start);
    res.drain(..).filter(|p| Path::new(&p).is_dir()).collect()
}

fn get_env_matches(start: &str) -> Vec<String> {
    let env_start = if start.starts_with('$') {
        &start[1..]
    } else {
        start
    };
    let mut ret: Vec<String> = Vec::new();
    for (key, _value) in env::vars() {
        if key.starts_with(env_start) {
            ret.push(format!("${}", key));
        }
    }
    ret
}

fn find_lisp_things(
    environment: &Environment,
    comps: &mut Vec<String>,
    start: &str,
    symbols: bool,
    need_quote: bool,
) {
    fn save_val(comps: &mut Vec<String>, data: &Expression, val: String, symbols: bool) {
        match data.get() {
            ExpEnum::Atom(Atom::Lambda(_)) => {
                if !symbols {
                    comps.push(val);
                }
            }
            ExpEnum::Atom(Atom::Macro(_)) => {
                if !symbols {
                    comps.push(val);
                }
            }
            ExpEnum::Function(_) => {
                if !symbols {
                    comps.push(val);
                }
            }
            _ => {
                if symbols {
                    comps.push(val);
                }
            }
        }
    }

    if start.contains("::") {
        // namespace reference.
        let mut key_i = start.splitn(2, "::");
        if let Some(namespace) = key_i.next() {
            if let Some(scope) = environment.namespaces.get(namespace) {
                if let Some(start) = key_i.next() {
                    let data = &scope.borrow().data;
                    for key in data.keys() {
                        if key.starts_with(start) {
                            let val = if need_quote {
                                format!("'{}::{}", namespace, key)
                            } else {
                                format!("{}::{}", namespace, key)
                            };
                            save_val(comps, &data.get(key).unwrap().exp, val, symbols);
                        }
                    }
                }
            }
        }
    } else {
        let mut loop_scope = Some(environment.current_scope.last().unwrap().clone());
        while let Some(scope) = loop_scope {
            let data = &scope.borrow().data;
            for key in data.keys() {
                if key.starts_with(start) {
                    let val = if need_quote {
                        format!("'{}", key)
                    } else {
                        (*key).to_string()
                    };
                    save_val(comps, &data.get(key).unwrap().exp, val, symbols);
                }
            }
            loop_scope = scope.borrow().outer.clone();
        }
    }
}

fn find_lisp_fns(environment: &Environment, comps: &mut Vec<String>, start: &str) {
    find_lisp_things(environment, comps, start, false, false)
}

fn find_lisp_symbols(environment: &Environment, comps: &mut Vec<String>, org_start: &str) {
    let (start, need_quote) = if org_start.starts_with('\'') {
        (&org_start[1..], true)
    } else {
        (org_start, false)
    };
    find_lisp_things(environment, comps, start, true, need_quote)
}

fn find_exes(comps: &mut Vec<String>, start: &str) {
    let paths = if let Some(paths) = env::var_os("PATH") {
        env::split_paths(&paths)
            .map(|s| {
                if !s.to_string_lossy().ends_with('/') {
                    let mut oss = s.into_os_string();
                    oss.push("/");
                    oss.into()
                } else {
                    s
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    for p in paths {
        if let Some(p) = p.to_str() {
            let pat = format!("{}*", p);
            match glob(&pat) {
                Ok(paths) => {
                    for p in paths {
                        match p {
                            Ok(p) => {
                                if let Some(p) = p.file_name() {
                                    if let Some(p) = p.to_str() {
                                        if p.starts_with(start) {
                                            comps.push(p.to_string());
                                        }
                                    }
                                }
                            }
                            Err(_err) => {}
                        }
                    }
                }
                Err(_err) => {}
            }
        }
    }
}
