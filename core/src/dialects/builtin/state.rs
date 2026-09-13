use std::any::Any;
use std::sync::Arc;

use crate::ty::TypeConstraint;
use crate::{Context, Error, IRFormatter, Type, TypeId, parse::Span};

use crate as tir;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StateResource {
    Memory,
    FpEnv,
}

impl StateResource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::FpEnv => "fp.env",
        }
    }

    pub fn semantic_code(self) -> u64 {
        match self {
            Self::Memory => tir_symbolic::lang::StateResourceKind::Memory as u64,
            Self::FpEnv => tir_symbolic::lang::StateResourceKind::FpEnvironment as u64,
        }
    }
}

/// An execution dependency for one resource, written `!state<resource>`.
pub struct StateType {
    resource: StateResource,
}

impl StateType {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(context: &Context, resource: StateResource) -> TypeId {
        let id = context.get_type_id(Arc::new(Self { resource }));
        if resource == StateResource::Memory {
            debug_assert_eq!(id, TypeId::STATE);
        }
        id
    }

    pub fn memory(context: &Context) -> TypeId {
        Self::new(context, StateResource::Memory)
    }

    pub fn fp_env(context: &Context) -> TypeId {
        Self::new(context, StateResource::FpEnv)
    }

    pub fn resource(&self) -> StateResource {
        self.resource
    }
}

impl TypeConstraint for StateType {}

impl Type for StateType {
    fn dialect(&self) -> &'static str {
        "builtin"
    }

    fn parse_key() -> &'static str {
        "state"
    }

    fn parse<'src>(
        _mnemonic: &str,
        parser: &mut tir::parse::text::Parser<'src>,
        context: &Context,
    ) -> Result<TypeId, (Span, Error)> {
        use tir::parse::common::Cursor;

        if !parser.parse_token("<") {
            return Err((parser.span(), Error::ExpectedToken("<")));
        }
        let resource = [StateResource::Memory, StateResource::FpEnv]
            .into_iter()
            .find(|resource| parser.parse_token(resource.name()))
            .ok_or_else(|| (parser.span(), Error::ExpectedToken("state resource")))?;
        if !parser.parse_token(">") {
            return Err((parser.span(), Error::ExpectedToken(">")));
        }
        Ok(Self::new(context, resource))
    }

    fn print(&self, fmt: &mut IRFormatter<'_>) -> Result<(), std::fmt::Error> {
        fmt.write(format!("state<{}>", self.resource.name()))
    }

    fn eq(&self, other: &dyn Type) -> bool {
        (other as &dyn Any)
            .downcast_ref::<StateType>()
            .is_some_and(|other| other.resource == self.resource)
    }

    fn hash(&self, state: &mut dyn std::hash::Hasher) {
        state.write_u8(self.resource as u8);
    }
}
