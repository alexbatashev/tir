mod arith;
mod func;
mod module;

use crate::dialect;

use crate as tir;

pub use arith::*;
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
            ConstantOp,
            AddIOp,
            SubIOp,
            MulIOp,
            AndIOp,
            OrIOp,
            XOrIOp,
            ShlIOp,
            ShrUIOp,
            ShrSIOp,
            CmpIOp,
            ExtSIOp,
            ExtUIOp,
            TruncIOp,
        ],
    }
}
