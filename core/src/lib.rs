mod block;
mod builder;
mod context;
mod dialect;
mod dialects;
mod error;
mod ir_formatter;
mod operation;
mod region;
mod value;
mod diagnostics;

pub mod helpers {
    pub use tir_macros::{dialect, operation};
}
pub mod parse;

pub use block::{Block, BlockId};
pub use builder::{IRBuilder, InsertionPoint};
pub use context::{Context, ContextIterator, ContextRef, GetFromContext};
pub use dialect::Dialect;
pub use error::Error;
pub use ir_formatter::IRFormatter;
pub use diagnostics::{print_error_range, print_parse_error};
pub use operation::{OpId, OpInstance, Operation};
pub use region::{Region, RegionId};
pub use value::{Use, Value};

pub use dialects::builtin;

pub use tir_macros::{dialect, operation};
