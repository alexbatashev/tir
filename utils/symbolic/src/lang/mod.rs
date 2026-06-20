use tir_adt::{APFloat, APInt};

mod infer;

pub use infer::infer_widths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SymKind {
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
    Not,
    /// Arguments are condition, then branch, else branch
    // #[arity = 3]
    If,
    // #[arity = 3]
    Clamp,
    /// Arguments are address, bytes read, signedness/address-space metadata.
    /// The third operand is nonsemantic for raw memory execution; explicit
    /// `SExt`/`ZExt` nodes model signedness.
    // #[arity = 3]
    LoadMemory,
    /// Arguments are address, bytes written, value, address-space metadata.
    // #[arity = 4]
    StoreMemory,
    ZExt,
    SExt,
    /// Bit-field extract: arguments are value, high bit, low bit (both inclusive).
    /// The result is the `high - low + 1` low bits. This is the single canonical
    /// representation of truncation/bit-slicing — there is deliberately no separate
    /// `Trunc` (`Trunc(x, n) == Extract(x, n-1, 0)`).
    // #[arity = 3]
    Extract,
    // #[arity = 1]
    Log2Ceil,
    // #[arity = 1]
    Sqrt,
    // #[arity = 3]
    Fma,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SymPayload<V> {
    SymbolId(u32),
    Value(V),
    Int(APInt),
    Float(APFloat),
}

/// A runtime value produced by the expression interpreter.
#[derive(Clone, Debug)]
pub enum Value {
    Int(APInt),
    Float(APFloat),
}
