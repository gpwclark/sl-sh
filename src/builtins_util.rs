use crate::environment::*;
use crate::eval::*;
use crate::types::*;
use std::borrow::Cow;

use std::convert::{TryFrom, TryInto};
use std::env;

pub fn param_eval_optional(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = Expression>,
) -> Result<Option<Expression>, LispError> {
    if let Some(arg) = args.next() {
        Ok(Some(eval(environment, arg)?))
    } else {
        Ok(None)
    }
}

pub fn param_eval(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = Expression>,
    form: &str,
) -> Result<Expression, LispError> {
    if let Some(arg) = param_eval_optional(environment, args)? {
        Ok(arg)
    } else {
        let msg = format!(
            "{}: Missing required argument, see (doc '{}) for usage.",
            form, form
        );
        Err(LispError::new(msg))
    }
}

pub fn params_done(
    args: &mut dyn Iterator<Item = Expression>,
    form: &str,
) -> Result<(), LispError> {
    if args.next().is_none() {
        Ok(())
    } else {
        let msg = format!(
            "{}: Too many arguments, see (doc '{}) for usage.",
            form, form
        );
        Err(LispError::new(msg))
    }
}

pub fn is_proper_list(exp: &Expression) -> bool {
    // does not detect empty (nil) lists on purpose.
    if let ExpEnum::Pair(_e1, e2) = &exp.get().data {
        if e2.is_nil() {
            true
        } else {
            is_proper_list(e2)
        }
    } else {
        false
    }
}

pub fn list_to_args(
    environment: &mut Environment,
    parts: &mut [Expression],
    do_eval: bool,
) -> Result<Vec<Expression>, LispError> {
    if do_eval {
        let mut args: Vec<Expression> = Vec::with_capacity(parts.len());
        for a in parts {
            args.push(eval(environment, a)?);
        }
        Ok(args)
    } else {
        let args: Vec<Expression> = parts.to_vec();
        Ok(args)
    }
}

pub fn parse_list_of_ints(
    environment: &mut Environment,
    args: &mut [Expression],
) -> Result<Vec<i64>, LispError> {
    let mut list: Vec<i64> = Vec::with_capacity(args.len());
    for arg in args {
        list.push(arg.make_int(environment)?);
    }
    Ok(list)
}

pub fn parse_list_of_floats(
    environment: &mut Environment,
    args: &mut [Expression],
) -> Result<Vec<f64>, LispError> {
    let mut list: Vec<f64> = Vec::with_capacity(args.len());
    for arg in args {
        list.push(arg.make_float(environment)?);
    }
    Ok(list)
}

pub fn parse_list_of_strings(
    environment: &mut Environment,
    args: &mut [Expression],
) -> Result<Vec<String>, LispError> {
    let mut list: Vec<String> = Vec::with_capacity(args.len());
    for arg in args {
        list.push(arg.make_string(environment)?);
    }
    Ok(list)
}

pub fn expand_tilde(path: &str) -> Option<String> {
    if path.ends_with('~') || path.contains("~/") {
        let mut home = match env::var("HOME") {
            Ok(val) => val,
            Err(_) => "/".to_string(),
        };
        if home.ends_with('/') {
            home.pop();
        }
        let mut new_path = String::new();
        let mut last_ch = ' ';
        for ch in path.chars() {
            if ch == '~' && (last_ch == ' ' || last_ch == ':') {
                if last_ch == '\\' {
                    new_path.push('~');
                } else {
                    new_path.push_str(&home);
                }
            } else {
                if last_ch == '\\' {
                    new_path.push('\\');
                }
                if ch != '\\' {
                    new_path.push(ch);
                }
            }
            last_ch = ch;
        }
        if last_ch == '\\' {
            new_path.push('\\');
        }
        Some(new_path)
    } else {
        None
    }
}

pub fn compress_tilde(path: &str) -> Option<String> {
    if let Ok(mut home) = env::var("HOME") {
        if home.ends_with('/') {
            home.pop();
        }
        if path.contains(&home) {
            Some(path.replace(&home, "~"))
        } else {
            None
        }
    } else {
        None
    }
}

pub fn make_args(
    environment: &mut Environment,
    args: &mut dyn Iterator<Item = Expression>,
) -> Result<Vec<Expression>, LispError> {
    let mut list: Vec<Expression> = Vec::new();
    for arg in args {
        list.push(eval(environment, arg)?);
    }
    Ok(list)
}

pub trait ExpandVecToArgs<Args> {
    type Output;
    fn call_expand_args(&self, args: Args) -> Self::Output;
}

impl<F, T, R> ExpandVecToArgs<[T; 0]> for F
where
    F: Fn() -> R,
{
    type Output = R;
    fn call_expand_args(&self, _args: [T; 0]) -> R {
        self()
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 1]> for F
where
    F: Fn(T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 1]) -> R {
        let [arg0] = args;
        self(arg0)
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 2]> for F
where
    F: Fn(T, T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 2]) -> R {
        let [arg0, arg1] = args;
        self(arg0, arg1)
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 3]> for F
where
    F: Fn(T, T, T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 3]) -> R {
        let [arg0, arg1, arg2] = args;
        self(arg0, arg1, arg2)
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 4]> for F
where
    F: Fn(T, T, T, T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 4]) -> R {
        let [arg0, arg1, arg2, arg3] = args;
        self(arg0, arg1, arg2, arg3)
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 5]> for F
where
    F: Fn(T, T, T, T, T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 5]) -> R {
        let [arg0, arg1, arg2, arg3, arg4] = args;
        self(arg0, arg1, arg2, arg3, arg4)
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 6]> for F
where
    F: Fn(T, T, T, T, T, T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 6]) -> R {
        let [arg0, arg1, arg2, arg3, arg4, arg5] = args;
        self(arg0, arg1, arg2, arg3, arg4, arg5)
    }
}

impl<F, T, R> ExpandVecToArgs<[T; 7]> for F
where
    F: Fn(T, T, T, T, T, T, T) -> R,
{
    type Output = R;
    fn call_expand_args(&self, args: [T; 7]) -> R {
        let [arg0, arg1, arg2, arg3, arg4, arg5, arg6] = args;
        self(arg0, arg1, arg2, arg3, arg4, arg5, arg6)
    }
}

pub trait TryIntoExpression<T>: Sized
where
    Self: ToString + TryInto<T>,
{
    type Error;

    fn human_readable_dest_type(&self) -> String;

    fn try_into_for(self, fn_name: &str) -> Result<T, LispError> {
        let hr_src_type = self.to_string();
        let hr_dest_type = self.human_readable_dest_type();
        let t = self.try_into();
        match t {
            Ok(t) => Ok(t),
            Err(_) => Err(LispError::new(format!(
                "{}: mismatched type input, expected {}, got {}.",
                fn_name, hr_dest_type, hr_src_type,
            ))),
        }
    }
}

impl TryIntoExpression<Expression> for Expression {
    type Error = LispError;

    fn human_readable_dest_type(&self) -> String {
        self.display_type()
    }
}

impl TryIntoExpression<String> for Expression {
    type Error = LispError;

    fn human_readable_dest_type(&self) -> String {
        ExpEnum::String(Cow::from(String::default()), Default::default()).to_string()
    }
}

impl From<String> for Expression {
    fn from(src: String) -> Self {
        Expression::alloc_data(ExpEnum::String(src.into(), None))
    }
}

impl TryFrom<Expression> for String {
    type Error = LispError;

    fn try_from(value: Expression) -> Result<Self, Self::Error> {
        match &value.get().data {
            ExpEnum::String(cow, _) => Ok(cow.to_string()),
            _ => Err(LispError::new(
                "Can only convert String from ExpEnum::String.",
            )),
        }
    }
}

impl From<bool> for Expression {
    fn from(val: bool) -> Self {
        if val {
            Expression::make_true()
        } else {
            Expression::make_false()
        }
    }
}

impl TryFrom<Expression> for bool {
    type Error = LispError;
    fn try_from(num: Expression) -> Result<Self, Self::Error> {
        match num.get().data {
            ExpEnum::True => Ok(true),
            ExpEnum::False => Ok(false),
            _ => Err(LispError::new(
                "Can only convert bool from ExpEnum::True or ExpEnum::False.",
            )),
        }
    }
}

impl TryIntoExpression<bool> for Expression {
    type Error = LispError;

    fn human_readable_dest_type(&self) -> String {
        ExpEnum::True.to_string() + " or " + &ExpEnum::False.to_string()
    }
}

impl From<f64> for Expression {
    fn from(num: f64) -> Self {
        Expression::alloc_data(ExpEnum::Float(num))
    }
}

impl TryFrom<Expression> for f64 {
    type Error = LispError;
    fn try_from(num: Expression) -> Result<Self, Self::Error> {
        match num.get().data {
            ExpEnum::Float(num) => Ok(num),
            ExpEnum::Int(num) => Ok(num as f64),
            _ => Err(LispError::new(
                "Can only convert f64 from ExpEnum::Float or ExpEnum::Int.",
            )),
        }
    }
}

impl TryIntoExpression<f64> for Expression {
    type Error = LispError;

    fn human_readable_dest_type(&self) -> String {
        ExpEnum::Float(Default::default()).to_string()
    }
}

impl From<i64> for Expression {
    fn from(num: i64) -> Self {
        Expression::alloc_data(ExpEnum::Int(num))
    }
}

impl TryFrom<Expression> for i64 {
    type Error = LispError;
    fn try_from(num: Expression) -> Result<Self, Self::Error> {
        match num.get().data {
            ExpEnum::Float(num) => Ok(num as i64),
            ExpEnum::Int(num) => Ok(num),
            _ => Err(LispError::new(
                "Can only convert i64 from ExpEnum::Float or ExpEnum::Int.",
            )),
        }
    }
}

impl TryIntoExpression<i64> for Expression {
    type Error = LispError;

    fn human_readable_dest_type(&self) -> String {
        ExpEnum::Int(Default::default()).to_string()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::convert::TryFrom;
    use std::convert::TryInto;

    #[test]
    fn test_from_numerical() {
        let result = Expression::from(42i64);
        assert_eq!(result, Expression::alloc_data(ExpEnum::Int(42i64)));

        let result: Expression = 42i64.into();
        assert_eq!(result, Expression::alloc_data(ExpEnum::Int(42i64)));

        let result = i64::try_from(Expression::alloc_data(ExpEnum::Int(7))).unwrap();
        assert_eq!(7, result);

        let result: f64 = Expression::alloc_data(ExpEnum::Int(7)).try_into().unwrap();
        assert_eq!(7f64, result);

        let result: f64 = Expression::alloc_data(ExpEnum::Float(7f64))
            .try_into()
            .unwrap();
        assert_eq!(7f64, result);

        let result: f64 = Expression::alloc_data(ExpEnum::Float(7f64))
            .try_into()
            .unwrap();
        assert_eq!(7f64, result);

        let result = f64::try_from(Expression::alloc_data(ExpEnum::Int(7i64))).unwrap();
        assert_eq!(7f64, result);
    }

    impl PartialEq<Self> for Expression {
        fn eq(&self, other: &Self) -> bool {
            match (&self.get().data, &other.get().data) {
                (ExpEnum::True, ExpEnum::True) => true,
                (ExpEnum::False, ExpEnum::False) => true,
                (ExpEnum::Nil, ExpEnum::Nil) => true,
                (ExpEnum::Float(lf), ExpEnum::Float(rt)) => lf == rt,
                (ExpEnum::Int(lf), ExpEnum::Int(rt)) => lf == rt,
                (ExpEnum::Char(lf), ExpEnum::Char(rt)) => lf == rt,
                (ExpEnum::CodePoint(lf), ExpEnum::CodePoint(rt)) => lf == rt,
                (_, _) => false,
            }
            //(ExpEnum::String(lf_s, lf_i), ExpEnum::String(rt_s, rt_i) => {}
            //(ExpEnum::Symbol(_, _), ExpEnum::Symbol(_, _)) => {}
            //(ExpEnum::Regex(_), ExpEnum::Regex(_)) => {}
            //ExpEnum::Symbol(_, _) => {}
            //ExpEnum::Lambda(_) => {}
            //ExpEnum::Macro(_) => {}
            //ExpEnum::Function(_) => {}
            //ExpEnum::LazyFn(_, _) => {}
            //ExpEnum::Vector(_) => {}
            //ExpEnum::Values(_) => {}
            //ExpEnum::Pair(_, _) => {}
            //ExpEnum::HashMap(_) => {}
            //ExpEnum::Process(_) => {}
            //ExpEnum::File(_) => {}
            //ExpEnum::Wrapper(_) => {}
            //ExpEnum::DeclareDef => {}
            //ExpEnum::DeclareVar => {}
            //ExpEnum::DeclareFn => {}
            //ExpEnum::DeclareMacro => {}
            //ExpEnum::Quote => {}
            //ExpEnum::BackQuote => {}
            //ExpEnum::Undefined => {}
        }
    }
}
