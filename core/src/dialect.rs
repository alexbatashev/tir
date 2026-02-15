use std::any::Any;
use std::sync::Arc;

use crate::parse::Span;
use crate::parse::text::Parser as IRParser;
use crate::ty::TypeParser;
use crate::{Context, Error, OpInstance, Operation};

pub trait Dialect: 'static + Send + Sync + Any {
    fn new() -> Arc<dyn Dialect>
    where
        Self: Sized;

    fn name() -> &'static str
    where
        Self: Sized;

    fn register_operations(&mut self, context: &Context);
    fn register_types(&mut self, context: &Context);

    fn get_dyn_op(&self, op: Arc<OpInstance>) -> Box<dyn Operation>;

    fn get_parser(
        &self,
        name: &str,
    ) -> Result<fn(&mut IRParser, &Context) -> Result<Box<dyn Operation>, (Span, Error)>, Error>;

    fn get_type_parser(&self, name: &str) -> Result<TypeParser, Error>;
}
