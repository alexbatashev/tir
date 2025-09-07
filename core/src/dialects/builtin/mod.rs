mod module;

use crate::dialect;

use crate as tir;

pub use module::*;

dialect! {
    BuiltinDialect {
        name: "builtin",
        operations: [
            ModuleOp,
            ModuleEndOp,
        ],
    }
}
