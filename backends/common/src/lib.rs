use tir::helpers::dialect;

pub mod isel;
mod lexer;
mod operations;
mod parser;

pub use operations::*;

pub use lexer::Token;
pub use lexer::lex;
pub use parser::{AsmInstructionParser, AsmParser};

pub mod ops {
    pub use crate::operations::*;
}

dialect! {
    AsmDialect {
        name: "asm",
        operations: [SectionOp, SectionEndOp, SymbolOp, SymbolEndOp, BlockEndOp],
    }
}
