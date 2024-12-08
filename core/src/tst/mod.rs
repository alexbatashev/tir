use crate::Dialect;
use crate::Ty;

mod ops;

pub use ops::*;

use tir_macros::dialect;
use tir_macros::populate_dialect_ops;
use tir_macros::populate_dialect_types;

use crate::assembly::OpAssembly;
use crate::assembly::TyAssembly;
use crate::Op;

dialect!(tst);
populate_dialect_ops!(TestOp);
populate_dialect_types!();
