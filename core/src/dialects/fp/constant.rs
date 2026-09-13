use tir_adt::APFloat;

use crate::builtin::FloatType;
use crate::{Context, Error, Operation, operation};

use crate as tir;

operation! {
    ConstantOp {
        name: "constant",
        dialect: "fp",
        attributes: A {
            bits: "UInt",
        },
        results: R {
            result: "FloatType",
        },
        interfaces: [crate::interp::Interp, crate::Speculatable],
        verifier: "true",
    }
}

impl crate::Speculatable for ConstantOp {}

impl tir::Verifiable for ConstantOp {
    fn verify_impl(&self, context: &Context) -> Result<(), Error> {
        let (_, _, width) = float_format(context, self.result())?;
        if width == 32 && self.bits() > u32::MAX as u64 {
            return Err(Error::VerificationError(
                "fp.constant bits do not fit binary32".into(),
            ));
        }
        Ok(())
    }
}

impl crate::interp::Interp for ConstantOp {
    fn evaluate(
        &self,
        _operands: &[crate::interp::Value],
        _state: &mut crate::interp::ExecutionState,
    ) -> Result<Vec<crate::interp::Value>, crate::interp::InterpError> {
        let context = self.handle().context.clone();
        let (exponent, mantissa, _) = float_format(&context, self.result())
            .map_err(|error| crate::interp::InterpError::Message(error.to_string()))?;
        Ok(vec![crate::interp::Value::Float(APFloat::from_bits(
            exponent,
            mantissa,
            false,
            self.bits() as u128,
        ))])
    }
}

fn float_format(context: &Context, value: crate::ValueId) -> Result<(u32, u32, u32), Error> {
    let ty = context.get_type_data(context.get_value(value).ty());
    (ty.as_ref() as &dyn std::any::Any)
        .downcast_ref::<FloatType>()
        .filter(|ty| matches!(ty.bit_width(), 32 | 64))
        .map(|ty| (ty.exp_width(), ty.mant_width(), ty.bit_width()))
        .ok_or_else(|| {
            Error::VerificationError("fp.constant supports only binary32 and binary64".into())
        })
}
