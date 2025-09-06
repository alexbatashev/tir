use crate::{
    Context, ContextIterator, Error, GetFromContext, Value,
    context::ContextRef,
    ir_formatter::IRFormatter,
    parse::Span,
    parse::text::Parser as IRParser,
    region::RegionId,
    value::ValueId,
};
use std::{any::Any, sync::Arc, u32};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(u32);

/// Core trait for all operations in TIR’s intermediate representation.
///
/// An `Operation` is the fundamental building block of the IR graph,
/// forming nodes that represent everything from high-level constructs to low-level code.
/// Each operation models a transformation or computation in the program,
/// and is used to describe program modules, functions, control flow, arithmetic, memory access,
/// and target-specific instructions.
///
/// All language constructs, from an entire module to a single arithmetic instruction,
/// are expressed as operations. This unified abstraction allows for powerful analyses,
/// transformations, and extension with new operation kinds through custom dialects.
///
/// # Example
///
/// Defining and using a custom operation:
/// ```rust
/// use tir_macros::operation;
///
/// operation! {
///     BarOp {
///         name: "bar",
///         dialect: "foo",
///     }
/// }
/// ```
///
/// This macro will generate a BarOp structure, as well as BarOpBuilder for constructing
/// custom operation.
///
/// Because all operations implement this trait, generic IR passes can inspect,
/// transform, or analyze any construct in the IR using the same programming model.
pub trait Operation: 'static + Send + Sync + Any {
    fn name() -> &'static str
    where
        Self: Sized;
    fn dialect() -> &'static str
    where
        Self: Sized;

    fn id(&self) -> OpId;

    fn from_op_instance(instance: Arc<OpInstance>) -> Self
    where
        Self: Sized;

    fn from_op_instance_dyn(instance: Arc<OpInstance>) -> Box<dyn Operation>
    where
        Self: Sized;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    fn print<'a, 'b: 'a>(&'a self, fmt: &'a mut IRFormatter<'b>) -> Result<(), std::fmt::Error>;
    fn parse<'src>(
        parser: &mut IRParser<'src>,
        context: &Context,
    ) -> Result<Box<dyn Operation>, (Span, Error)>
    where
        Self: Sized;

    fn regions(&self) -> ContextIterator<RegionId>;
    fn operands(&self) -> &[Value];
}

#[derive(Debug, Clone)]
pub struct OpInstance {
    pub id: OpId,
    pub name: &'static str,
    pub dialect: &'static str,
    pub context: ContextRef,
    pub operands: Vec<ValueId>,
    pub results: Vec<ValueId>,
    pub regions: Vec<RegionId>,
}

impl OpInstance {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn dialect(&self) -> &'static str {
        self.dialect
    }

    pub fn as_op<T: Operation + Sized>(self: Arc<Self>) -> Option<T> {
        if self.name == T::name() {
            Some(T::from_op_instance(self))
        } else {
            None
        }
    }

    pub fn as_dyn_op(self: Arc<Self>) -> Box<dyn Operation> {
        let context = self.context.upgrade();
        context.get_dyn_op(self.clone())
    }
}

impl Default for OpId {
    fn default() -> Self {
        Self(u32::MAX)
    }
}

impl OpId {
    pub fn invalid() -> Self {
        Self::default()
    }

    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }
}

impl GetFromContext for OpId {
    type Item = Arc<OpInstance>;

    fn get_from_context(&self, context: &crate::Context) -> Self::Item {
        context.get_op(*self)
    }
}
