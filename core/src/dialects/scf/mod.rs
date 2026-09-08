use crate::{Terminator, dialect, operation};

use crate as tir;
use crate::Any as AnyConstraint;

pub mod nodes;
pub use nodes::{ForOp, ForOpBuilder, LoopOp, LoopOpBuilder, SwitchOp, SwitchOpBuilder};

pub mod ops {
    pub use super::nodes::{r#for, r#loop, switch};
    pub use super::{YieldOp, r#yield};
}

dialect! {
    ScfDialect {
        name: "scf",
        operations: [
            LoopOp,
            SwitchOp,
            ForOp,
            YieldOp,
        ],
        types: [],
    }
}

// The back edge of an `scf.for` whose body is still a block list: it names
// what the next iteration carries. An unordered body names its results
// outright and has no terminator, so nothing else needs this.
operation! {
    YieldOp {
        name: "yield",
        dialect: "scf",
        operands: O {
            values: "*AnyConstraint",
        },
        interfaces: [Terminator],
    }
}

impl Terminator for YieldOp {}
