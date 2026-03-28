use crate::{
    Context,
    graph::NodeKind,
    sem_expr::{APFloat, APInt},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExprKind {
    Symbol,
    Constant,
    Add,
    Sub,
    Mul,
    Div,
    UDiv,
    Eq,
    Ne,
    Lt,
    Gt,
    Ge,
    ULt,
    ULe,
    UGt,
    UGe,
    ShiftLeft,
    ShiftRightArithmetic,
    ShiftRightLogic,
    Or,
    And,
    Xor,
    If,
    LoadMemory,
    StoreMemory,
    ZExt,
    SExt,
    Log2Ceil,
    Sqrt,
    Fma,
}

pub enum ExprPayload {
    SymbolId(u32),
    Int(APInt),
    Float(APFloat),
}

impl NodeKind for ExprKind {
    fn is_leaf(&self, _: &Context) -> bool {
        match self {
            ExprKind::Constant | ExprKind::Symbol => true,
            _ => false,
        }
    }

    fn num_children(&self, _: &Context) -> usize {
        match self {
            ExprKind::Constant | ExprKind::Symbol => 0,
            ExprKind::If | ExprKind::Fma => 3,
            ExprKind::Sqrt | ExprKind::Log2Ceil => 1,
            _ => 2,
        }
    }
}
