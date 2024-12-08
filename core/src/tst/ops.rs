use crate::tst::DIALECT_NAME;
use tir_macros::operation;

use crate as tir_core;

#[operation(name = "test", dialect = tst, known_attrs(value: IntegerAttr))]
#[derive(Op, OpAssembly, Clone, OpValidator)]
pub struct TestOp {}
