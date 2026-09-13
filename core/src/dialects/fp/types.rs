use std::any::Any;
use std::sync::Arc;

use crate::ty::TypeConstraint;
use crate::{Context, Error, IRFormatter, Type, TypeId, parse::Span};

macro_rules! scalar_type {
    ($name:ident, $mnemonic:literal) => {
        pub struct $name;

        impl $name {
            #[allow(clippy::new_ret_no_self)]
            pub fn new(context: &Context) -> TypeId {
                context.get_type_id(Arc::new(Self))
            }
        }

        impl TypeConstraint for $name {}

        impl Type for $name {
            fn dialect(&self) -> &'static str {
                "fp"
            }
            fn parse_key() -> &'static str {
                $mnemonic
            }
            fn parse<'src>(
                _mnemonic: &str,
                _parser: &mut crate::parse::text::Parser<'src>,
                context: &Context,
            ) -> Result<TypeId, (Span, Error)> {
                Ok(Self::new(context))
            }
            fn print(&self, fmt: &mut IRFormatter<'_>) -> Result<(), std::fmt::Error> {
                fmt.write($mnemonic)
            }
            fn eq(&self, other: &dyn Type) -> bool {
                (other as &dyn Any).downcast_ref::<Self>().is_some()
            }
            fn hash(&self, _state: &mut dyn std::hash::Hasher) {}
        }
    };
}

scalar_type!(EnvironmentType, "env");
scalar_type!(RoundingType, "rounding");
