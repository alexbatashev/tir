use std::any::Any;
use std::sync::Arc;

use crate::parse::common::Cursor;
use crate::ty::TypeConstraint;
use crate::{Context, Error, IRFormatter, Operation, Type, TypeId, operation, parse::Span};

use crate as tir;
use crate::Any as AnyConstraint;

pub struct TupleType {
    elements: Vec<Arc<dyn Type>>,
}

impl TupleType {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(context: &Context, elements: Vec<TypeId>) -> TypeId {
        let elements = elements
            .into_iter()
            .map(|element| context.get_type_data(element))
            .collect();
        context.get_type_id(Arc::new(Self { elements }))
    }

    pub fn elements(&self, context: &Context) -> Vec<TypeId> {
        self.elements
            .iter()
            .map(|element| context.get_type_id(element.clone()))
            .collect()
    }
}

impl TypeConstraint for TupleType {}

impl Type for TupleType {
    fn dialect(&self) -> &'static str {
        "builtin"
    }

    fn parse_key() -> &'static str {
        "tuple"
    }

    fn parse<'src>(
        _mnemonic: &str,
        parser: &mut crate::parse::text::Parser<'src>,
        context: &Context,
    ) -> Result<TypeId, (Span, Error)> {
        let elements = parser
            .parse_delimited("<", ">", |parser| {
                parser
                    .parse_type(context)?
                    .ok_or_else(|| (parser.span(), Error::ExpectedType))
            })?
            .ok_or_else(|| (parser.span(), Error::ExpectedToken("<")))?;

        Ok(Self::new(context, elements))
    }

    fn print(&self, fmt: &mut IRFormatter<'_>) -> Result<(), std::fmt::Error> {
        fmt.write("tuple<")?;
        for (index, element) in self.elements.iter().enumerate() {
            if index > 0 {
                fmt.write(", ")?;
            }
            fmt.write("!")?;
            if element.dialect() != "builtin" {
                fmt.write(format!("{}.", element.dialect()))?;
            }
            element.print(fmt)?;
        }
        fmt.write(">")
    }

    fn eq(&self, other: &dyn Type) -> bool {
        let Some(other) = (other as &dyn Any).downcast_ref::<TupleType>() else {
            return false;
        };
        self.elements.len() == other.elements.len()
            && self
                .elements
                .iter()
                .zip(&other.elements)
                .all(|(left, right)| left.eq(right.as_ref()))
    }

    fn hash(&self, state: &mut dyn std::hash::Hasher) {
        for element in &self.elements {
            state.write_usize(Arc::as_ptr(element) as *const () as usize);
        }
        state.write_usize(self.elements.len());
    }
}

operation! {
    MakeTupleOp {
        name: "make_tuple",
        dialect: "builtin",
        verifier: "true",
        operands: O {
            elements: "*AnyConstraint",
        },
        results: R {
            result: "crate::builtin::TupleType",
        },
        interfaces: [crate::interp::Interp, crate::Speculatable],
    }
}

impl crate::Speculatable for MakeTupleOp {}

impl tir::Verifiable for MakeTupleOp {
    fn verify_impl(&self, context: &Context) -> Result<(), Error> {
        let result_type = context.get_type_data(context.get_value(self.result()).ty());
        let Some(tuple) = (result_type.as_ref() as &dyn Any).downcast_ref::<TupleType>() else {
            return Err(Error::VerificationError(
                "make_tuple result must have tuple type".to_string(),
            ));
        };
        let expected = tuple.elements(context);
        if expected.len() != self.operands().len() {
            return Err(Error::VerificationError(format!(
                "make_tuple expected {} elements, got {}",
                expected.len(),
                self.operands().len()
            )));
        }
        for (index, (&value, expected_type)) in self.operands().iter().zip(expected).enumerate() {
            if context.get_value(value).ty() != expected_type {
                return Err(Error::VerificationError(format!(
                    "make_tuple element {index} has the wrong type"
                )));
            }
        }
        Ok(())
    }
}

operation! {
    TupleGetOp {
        name: "tuple_get",
        dialect: "builtin",
        verifier: "true",
        operands: O {
            tuple: "crate::builtin::TupleType",
        },
        attributes: A {
            index: "UInt",
        },
        results: R {
            result: "AnyConstraint",
        },
        interfaces: [crate::interp::Interp, crate::Speculatable],
    }
}

impl crate::Speculatable for TupleGetOp {}

impl TupleGetOp {
    pub fn tuple(&self) -> crate::ValueId {
        self.operands()[0]
    }

    pub fn index(&self) -> usize {
        match self.attr("index") {
            Some(crate::attributes::AttributeValue::UInt(index)) => index as usize,
            _ => panic!("tuple_get must carry an index"),
        }
    }
}

impl tir::Verifiable for TupleGetOp {
    fn verify_impl(&self, context: &Context) -> Result<(), Error> {
        let tuple_type = context.get_type_data(context.get_value(self.tuple()).ty());
        let Some(tuple) = (tuple_type.as_ref() as &dyn Any).downcast_ref::<TupleType>() else {
            return Err(Error::VerificationError(
                "tuple_get operand must have tuple type".to_string(),
            ));
        };
        let elements = tuple.elements(context);
        let Some(&expected) = elements.get(self.index()) else {
            return Err(Error::VerificationError(format!(
                "tuple_get index {} is out of bounds for {} elements",
                self.index(),
                elements.len()
            )));
        };
        if context.get_value(self.result()).ty() != expected {
            return Err(Error::VerificationError(
                "tuple_get result has the wrong type".to_string(),
            ));
        }
        Ok(())
    }
}
