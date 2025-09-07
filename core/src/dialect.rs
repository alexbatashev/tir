use std::any::Any;
use std::sync::Arc;

use crate::parse::Span;
use crate::parse::text::Parser as IRParser;
use crate::{Context, Error, OpInstance, Operation};

pub trait Dialect: 'static + Send + Sync + Any {
    fn new() -> Arc<dyn Dialect>
    where
        Self: Sized;

    fn name() -> &'static str
    where
        Self: Sized;

    fn register_operations(&mut self, context: &Context);

    fn get_dyn_op(&self, op: Arc<OpInstance>) -> Box<dyn Operation>;

    fn get_parser(
        &self,
        name: &str,
    ) -> Result<fn(&mut IRParser, &Context) -> Result<Box<dyn Operation>, (Span, Error)>, Error>;

    fn parse_native(&self, _source: &str) -> Box<dyn Operation> {
        unimplemented!("This dialect does not support native parsing")
    }
}
