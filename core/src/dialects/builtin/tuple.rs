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
        if !parser.parse_token("<") {
            return Err((parser.span(), Error::ExpectedToken("<")));
        }

        let mut elements = vec![];
        if !parser.parse_token(">") {
            loop {
                elements.push(
                    parser
                        .parse_type(context)?
                        .ok_or_else(|| (parser.span(), Error::ExpectedType))?,
                );
                if parser.parse_token(">") {
                    break;
                }
                if !parser.parse_token(",") {
                    return Err((parser.span(), Error::ExpectedToken(",")));
                }
            }
        }

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
}

operation! {
    MakeTupleOp {
        name: "make_tuple",
        dialect: "builtin",
        format: "custom",
        verifier: "true",
        operands: O {
            elements: "*AnyConstraint",
        },
        results: R {
            result: "crate::builtin::TupleType",
        },
    }
}

impl MakeTupleOp {
    fn custom_print(&self, fmt: &mut IRFormatter) -> Result<(), std::fmt::Error> {
        let context = self.0.context.upgrade();
        fmt.write(format!("%{} = make_tuple", self.result().number()))?;
        for (index, element) in self.operands().iter().enumerate() {
            if index == 0 {
                fmt.write(" ")?;
            } else {
                fmt.write(", ")?;
            }
            fmt.write(format!("%{}", element.number()))?;
        }
        fmt.write(" : ")?;
        context.print_type(context.get_value(self.result()).ty(), fmt)?;
        fmt.write("\n")
    }

    fn custom_parse(
        parser: &mut crate::parse::text::Parser,
        context: &Context,
    ) -> Result<Box<dyn Operation>, (Span, Error)> {
        let mut elements = vec![];
        let mut next = parser.parse_value_ref();
        while let Some(reference) = next {
            elements.push(
                parser.resolve_value(reference).ok_or_else(|| {
                    (parser.span(), Error::UnknownValueRef(reference.to_string()))
                })?,
            );
            if !parser.parse_token(",") {
                break;
            }
            next = Some(
                parser
                    .parse_value_ref()
                    .ok_or_else(|| (parser.span(), Error::ExpectedValueRef))?,
            );
        }
        if !parser.parse_token(":") {
            return Err((parser.span(), Error::ExpectedToken(":")));
        }
        let result_type = parser
            .parse_type(context)?
            .ok_or_else(|| (parser.span(), Error::ExpectedType))?;

        Ok(Box::new(
            MakeTupleOpBuilder::new(context)
                .elements(elements)
                .result_type(result_type)
                .build(),
        ))
    }
}

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
    }
}

impl TupleGetOp {
    pub fn tuple(&self) -> crate::ValueId {
        self.operands()[0]
    }

    pub fn index(&self) -> usize {
        self.attributes()
            .iter()
            .find_map(|attribute| match attribute.value {
                crate::attributes::AttributeValue::UInt(index) if attribute.name == "index" => {
                    Some(index as usize)
                }
                _ => None,
            })
            .expect("tuple_get must carry an index")
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

#[cfg(test)]
mod tests {
    use crate::{Context, Operation, builtin::IntegerType};

    #[test]
    fn tuple_construction_preserves_element_order() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let i64_ty = IntegerType::new(&context, 64);
        let left = context.create_value(i32_ty, None);
        let right = context.create_value(i64_ty, None);
        let (left_id, right_id) = (left.id(), right.id());
        let _block = context.create_block(vec![left, right]);
        let tuple_ty = super::TupleType::new(&context, vec![i32_ty, i64_ty]);

        let tuple = super::MakeTupleOpBuilder::new(&context)
            .elements(vec![left_id, right_id])
            .result_type(tuple_ty)
            .build();

        assert_eq!(tuple.operands(), &[left_id, right_id]);
        assert_eq!(context.get_value(tuple.result()).ty(), tuple_ty);
    }

    #[test]
    fn tuple_extraction_selects_one_element() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let i64_ty = IntegerType::new(&context, 64);
        let tuple_ty = super::TupleType::new(&context, vec![i32_ty, i64_ty]);
        let tuple = context.create_value(tuple_ty, None);
        let tuple_id = tuple.id();
        let _block = context.create_block(vec![tuple]);

        let get = super::TupleGetOpBuilder::new(&context)
            .tuple(tuple_id)
            .attr("index", crate::attributes::AttributeValue::UInt(1))
            .result_type(i64_ty)
            .build();

        assert_eq!(get.tuple(), tuple_id);
        assert_eq!(get.index(), 1);
        assert_eq!(context.get_value(get.result()).ty(), i64_ty);
    }
}
