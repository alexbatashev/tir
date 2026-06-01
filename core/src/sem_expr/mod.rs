use crate::{
    Operation, ValueId,
    graph::{MutDag, NodeId, PostOrderDag},
    helpers::SimpleNode,
    utils::{APFloat, APInt},
};

mod exec;
mod infer;

pub use exec::execute;
pub use infer::{canonicalize_for_selection, infer_widths};

pub type ExprPostGraph = PostOrderDag<ExprKind, ExprPayload>;

pub trait AsSemExpr: Operation {
    fn convert(&self, g: &mut impl MutDag<Node = ExprKind, Leaf = ExprPayload>) -> NodeId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, SimpleNode)]
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
    /// Bit-field extract: arguments are value, high bit, low bit (both inclusive).
    /// The result is the `high - low + 1` low bits. This is the single canonical
    /// representation of truncation/bit-slicing — there is deliberately no separate
    /// `Trunc` (`Trunc(x, n) == Extract(x, n-1, 0)`).
    #[arity = 3]
    Extract,
    #[arity = 1]
    Log2Ceil,
    #[arity = 1]
    Sqrt,
    #[arity = 3]
    Fma,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprPayload {
    SymbolId(u32),
    Value(ValueId),
    Int(APInt),
    Float(APFloat),
}

/// A runtime value produced by the expression interpreter.
#[derive(Clone, Debug)]
pub enum Value {
    Int(APInt),
    Float(APFloat),
}
