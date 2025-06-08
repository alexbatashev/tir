use tir::helpers::dialect;

mod lexer;
mod operations;

pub use operations::{BlockEndOp, BlockEndOpBuilder, SectionOp, SectionOpBuilder};

dialect! {
    AsmDialect {
        name: "asm",
        operations: [SectionOp, BlockEndOp],
    }
}
