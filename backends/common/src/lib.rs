use tir::helpers::dialect;

mod lexer;
mod operations;
mod parser;

pub use operations::{BlockEndOp, BlockEndOpBuilder, SectionOp, SectionOpBuilder};

pub use lexer::lex;

dialect! {
    AsmDialect {
        name: "asm",
        operations: [SectionOp, BlockEndOp],
    }
}
