use crate::lisp_adapters::SlFrom;
use bridge_types::ErrorStrings;
use compile_state::state::SloshVm;
use slvm::value::ValueType;
use slvm::{from_i56, to_i56, VMError, VMResult, Value, ValueTypes, F56};

impl SlFrom<()> for Value {
    fn sl_from(_value: (), _vm: &mut SloshVm) -> VMResult<Self> {
        Ok(Value::Nil)
    }
}

impl SlFrom<&Value> for () {
    fn sl_from(value: &Value, _vm: &mut SloshVm) -> VMResult<()> {
        match value {
            Value::Nil => Ok(()),
            _ => Err(VMError::new_conversion(
                ErrorStrings::fix_me_mismatched_type(
                    <&'static str>::from(ValueType::Nil),
                    value.display_type(_vm),
                ),
            )),
        }
    }
}

impl SlFrom<i32> for Value {
    fn sl_from(value: i32, _vm: &mut SloshVm) -> VMResult<Self> {
        Ok(to_i56(value as i64))
    }
}
impl SlFrom<u32> for Value {
    fn sl_from(value: u32, _vm: &mut SloshVm) -> VMResult<Self> {
        Ok(to_i56(value as i64))
    }
}

impl SlFrom<&Value> for i32 {
    fn sl_from(value: &Value, vm: &mut SloshVm) -> VMResult<i32> {
        match value {
            Value::Int(num) => {
                let num = from_i56(num);
                num.try_into().map_err(|_| {
                    VMError::new_conversion(
                        "Provided slosh value too small to fit desired type.".to_string(),
                    )
                })
            }
            _ => Err(VMError::new_conversion(
                ErrorStrings::fix_me_mismatched_type(
                    <&'static str>::from(ValueType::Int),
                    value.display_type(vm),
                ),
            )),
        }
    }
}

impl SlFrom<f64> for Value {
    fn sl_from(value: f64, _vm: &mut SloshVm) -> VMResult<Self> {
        Ok(Value::Float(F56::from(value)))
    }
}

impl SlFrom<&Value> for f64 {
    fn sl_from(value: &Value, vm: &mut SloshVm) -> VMResult<Self> {
        match value {
            Value::Float(f56) => Ok(f64::from(*f56)),
            _ => Err(VMError::new_conversion(
                ErrorStrings::fix_me_mismatched_type(
                    <&'static str>::from(ValueType::Float),
                    value.display_type(vm),
                ),
            )),
        }
    }
}

impl SlFrom<&Value> for usize {
    fn sl_from(value: &Value, vm: &mut SloshVm) -> VMResult<Self> {
        match value {
            Value::Int(i) => usize::try_from(from_i56(i)).map_err(|_| {
                VMError::new_conversion(ErrorStrings::fix_me_mismatched_type(
                    <&'static str>::from(ValueType::Int),
                    value.display_type(vm),
                ))
            }),
            _ => Err(VMError::new_conversion(
                ErrorStrings::fix_me_mismatched_type(
                    String::from(ValueTypes::from([ValueType::Int])),
                    value.display_type(vm),
                ),
            )),
        }
    }
}

impl SlFrom<&Value> for i64 {
    fn sl_from(value: &Value, vm: &mut SloshVm) -> VMResult<Self> {
        match value {
            Value::Int(i) => Ok(from_i56(i)),
            _ => Err(VMError::new_conversion(
                ErrorStrings::fix_me_mismatched_type(
                    <&'static str>::from(ValueType::Int),
                    value.display_type(vm),
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lisp_adapters::SlFrom;
    use crate::lisp_adapters::SlInto;
    use compile_state::state::new_slosh_vm;
    use slvm::{to_i56, Value};

    #[test]
    fn test_i32_conversions_rust_to_value() {
        let mut vm = new_slosh_vm();
        let vm = &mut vm;
        let test_vals = vec![0_i32, 1_i32, -1_i32, i32::MIN, i32::MAX];
        for val in test_vals {
            let val: Value = val.sl_into(vm).expect("i32 can be converted to Value");
            assert!(matches!(val, Value::Int(_)));
        }
    }

    #[test]
    fn test_i32_conversions_value_to_rust() {
        let mut vm = new_slosh_vm();
        let vm = &mut vm;
        let val = to_i56(7_i32 as i64);
        let _val: i32 = i32::sl_from(&val, vm).expect("Value can be converted to i32");
    }
}
