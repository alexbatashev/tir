use std::any::Any;
use std::sync::Arc;

use tir::attributes::AttributeValue;
use tir::{
    Context, Error, IRFormatter, TirType, Type, TypeConstraint, TypeId, dialect, operation,
    parse::Span,
};

pub mod ops {
    pub use super::{StringOp, VaArgOp, VaEndOp, VaStartOp, string, va_arg, va_end, va_start};
}

dialect! {
    CirDialect {
        name: "cir",
        operations: [
            StringOp,
            VaStartOp,
            VaArgOp,
            VaEndOp,
        ],
        types: [VarArgsType, VaListType],
    }
}

#[derive(TirType)]
#[tir_type(dialect = "cir", name = "varargs")]
pub struct VarArgsType;

impl VarArgsType {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(context: &Context) -> TypeId {
        context.get_type_id(Arc::new(Self))
    }
}

impl TypeConstraint for VarArgsType {}

impl Type for VarArgsType {
    fn dialect(&self) -> &'static str {
        "cir"
    }

    fn parse_key() -> &'static str {
        "varargs"
    }

    fn parse<'src>(
        _mnemonic: &str,
        _parser: &mut tir::parse::text::Parser<'src>,
        context: &Context,
    ) -> Result<TypeId, (Span, Error)> {
        Ok(Self::new(context))
    }

    fn print(&self, fmt: &mut IRFormatter<'_>) -> Result<(), std::fmt::Error> {
        fmt.write("varargs")
    }

    fn eq(&self, other: &dyn Type) -> bool {
        (other as &dyn Any).downcast_ref::<VarArgsType>().is_some()
    }
}

#[derive(TirType)]
#[tir_type(dialect = "cir", name = "va_list")]
pub struct VaListType;

impl VaListType {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(context: &Context) -> TypeId {
        context.get_type_id(Arc::new(Self))
    }
}

impl TypeConstraint for VaListType {}

impl Type for VaListType {
    fn dialect(&self) -> &'static str {
        "cir"
    }

    fn parse_key() -> &'static str {
        "va_list"
    }

    fn parse<'src>(
        _mnemonic: &str,
        _parser: &mut tir::parse::text::Parser<'src>,
        context: &Context,
    ) -> Result<TypeId, (Span, Error)> {
        Ok(Self::new(context))
    }

    fn print(&self, fmt: &mut IRFormatter<'_>) -> Result<(), std::fmt::Error> {
        fmt.write("va_list")
    }

    fn eq(&self, other: &dyn Type) -> bool {
        (other as &dyn Any).downcast_ref::<VaListType>().is_some()
    }
}

operation! {
    StringOp {
        name: "string",
        dialect: "cir",
        attributes: A {
            value: "Str",
        },
        results: R {
            result: "tir::ptr::PtrType",
        },
    }
}

operation! {
    VaStartOp {
        name: "va_start",
        dialect: "cir",
        results: R {
            result: "crate::cir::VaListType",
        },
    }
}

operation! {
    VaArgOp {
        name: "va_arg",
        dialect: "cir",
        operands: O {
            list: "crate::cir::VaListType",
        },
        results: R {
            result: "tir::Any",
        },
    }
}

operation! {
    VaEndOp {
        name: "va_end",
        dialect: "cir",
        operands: O {
            list: "crate::cir::VaListType",
        },
    }
}

pub fn string_op(context: &Context, value: &str, result_type: TypeId) -> StringOp {
    StringOpBuilder::new(context)
        .attr("value", AttributeValue::Str(value.to_string()))
        .result_type(result_type)
        .build()
}
