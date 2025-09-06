use std::sync::Arc;

use crate::{Context, Error, OpInstance, Operation};
use crate::parse::Span;
use crate::parse::text::Parser as IRParser;

pub trait Dialect: 'static + Send + Sync {
    fn new() -> Box<dyn Dialect>
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
