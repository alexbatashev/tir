use crate::builtin::IntType;
use crate::tst::DIALECT_NAME;
use crate::Value;
use tir_macros::operation;

use crate as tir_core;

use tir_core::{Op, OpAssembly};

use tir_macros::OpAssembly;
use tir_macros::OpValidator;

#[operation(name = "test", path = tir_core::tst::ops, known_attrs(value: IntegerAttr))]
#[derive(Clone, OpValidator)]
pub struct TestOp {
    #[operand]
    op1: Value<IntType>,
}
