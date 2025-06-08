mod module;

use crate::dialect;

use crate as tir;

pub use module::{ModuleEndOp, ModuleEndOpBuilder, ModuleOp, ModuleOpBuilder};

dialect! {
    BuiltinDialect {
        name: "builtin",
        operations: [
            ModuleOp,
            ModuleEndOp,
        ],
    }
}
