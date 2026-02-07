mod func;
mod module;

use crate::dialect;

use crate as tir;

pub use func::*;
pub use module::*;

dialect! {
    BuiltinDialect {
        name: "builtin",
        operations: [
            ModuleOp,
            ModuleEndOp,
            FuncOp,
            ReturnOp,
        ],
    }
}
