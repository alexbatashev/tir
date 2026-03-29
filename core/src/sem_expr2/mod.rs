use crate::{
    graph::{MutDag, NodeId, PostOrderDag},
    helpers::SimpleNode,
    sem_expr::BitVec,
    utils::{APFloat, APInt},
};

mod exec;

pub use exec::execute;

pub type ExprPostGraph = PostOrderDag<ExprKind, ExprPayload>;

pub trait AsSemExpr {
    fn convert(&self, g: &mut impl MutDag<Node = ExprKind, Leaf = ExprPayload>) -> NodeId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, SimpleNode)]
#[repr(u16)]
#[simple_node(default_arity = 2)]
pub enum ExprKind {
    #[leaf]
    Symbol,
    #[leaf]
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
    /// Arguments are condition, then branch, else branch
    #[arity = 3]
    If,
    #[arity = 3]
    Clamp,
    /// Arguments are address space, address, bytes read
    #[arity = 3]
    LoadMemory,
    /// Arguments are address space, address, value, bytes written
    #[arity = 4]
    StoreMemory,
    ZExt,
    SExt,
    #[arity = 1]
    Log2Ceil,
    #[arity = 1]
    Sqrt,
    #[arity = 3]
    Fma,
}

pub enum ExprPayload {
    SymbolId(u32),
    Int(APInt),
    Float(APFloat),
    BitVec(BitVec),
}

/// A runtime value produced by the expression interpreter.
#[derive(Clone, Debug)]
pub enum Value {
    Int(APInt),
    Float(APFloat),
}
