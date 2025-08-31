use tir::helpers::{dialect, operation};

include!(concat!(env!("OUT_DIR"), "/riscv.rs"));

dialect! {
    RiscvDialect {
        name: "riscv",
        operations: [
            // RV32I
            AddOp,
            SubOp,
            ShiftLeftLogicalOp,
            ShiftRightLogicalOp,
            XorOp,
            AndOp,
            OrOp
        ],
    }
}
