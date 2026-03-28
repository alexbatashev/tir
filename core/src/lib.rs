extern crate self as tir;

pub mod attributes;
mod block;
mod builder;
mod context;
mod diagnostics;
mod dialect;
mod dialects;
mod error;
pub mod graph;
mod interfaces;
mod ir_formatter;
mod operand;
mod operation;
mod pass;
mod region;
pub mod sem_expr;
pub mod sem_expr2;
mod ty;
pub mod utils;
mod value;

pub mod helpers {
    pub use tir_macros::{SimpleNode, dialect, operation};
}
pub mod parse;

pub use block::{Block, BlockId};
pub use builder::{IRBuilder, InsertionPoint};
pub use context::{Context, ContextIterator, ContextRef, GetFromContext};
pub use diagnostics::{print_error_range, print_parse_error};
pub use dialect::Dialect;
pub use error::Error;
pub use interfaces::{Commutative, SameOperandType, Terminator};
pub use ir_formatter::IRFormatter;
pub use operand::Operand;
pub use operation::{
    ErasedOpInterface, ImplementsOpInterface, OpDefVerifiable, OpId, OpInstance,
    OpInterfaceConverter, Operation, Verifiable, downcast_op_interface, erase_op_interface,
    op_interface_converter,
};
pub use pass::{OperationRef, Pass, PassError, PassManager, PassTarget, Rewriter};
pub use region::{Region, RegionId};
pub use ty::{Any, Type, TypeConstraint, TypeId, TypeParser};
pub use value::{Use, Value, ValueId};

pub use dialects::builtin;
pub use dialects::builtin::Integer;

pub use tir_macros::{dialect, operation};
