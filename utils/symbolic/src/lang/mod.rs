use tir_adt::{APFloat, APInt, RawBits};
use tir_graph::Matchable;

mod exec;
mod infer;
mod sexpr;

pub use exec::{Memory, execute, execute_with_memory};
pub use infer::{canonicalize_for_selection, infer_widths};
pub use sexpr::{BuildError, SemBuilderHooks, SemExpr, build, parse};

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
    /// Signed and unsigned remainder (truncated division).
    SRem,
    URem,
    /// Unary two's-complement negation.
    Neg,
    Eq,
    Ne,
    Lt,
    Le,
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
    /// Bit concatenation: the result width is the sum of both operand widths,
    /// with the first operand occupying the high bits.
    Concat,
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
    /// Apply a function to each lane of an iterator. Arguments are `[iter, body]`:
    /// `body` is evaluated once per element with the element bound as the lambda's
    /// argument, read via `Arg(0)` (or `Arg(0)`/`Arg(1)` when the element is a pair
    /// produced by `Zip`). The node's value is the iterator of results.
    // #[arity = 2]
    Map,
    /// Pair two iterators lane-wise. Arguments are `[lhs, rhs]`; the value is an
    /// iterator whose element `i` is the two-element iterator `[lhs[i], rhs[i]]`.
    /// Feeding a `Zip` into a `Map` lets a binary lambda read both sides via
    /// `Arg(0)`/`Arg(1)`.
    // #[arity = 2]
    Zip,
    /// Concatenate the lanes of an iterator into a single bit value, lane 0 in the
    /// low bits. The inverse of `Split`. One argument: the iterator.
    // #[arity = 1]
    IterConcat,
    /// Split a bit value into `n` equal-width lanes. Arguments are `[bits, n]`;
    /// the value is an iterator of `n` elements, lane 0 taken from the low bits.
    /// The inverse of `IterConcat`.
    // #[arity = 2]
    Split,
    /// Left-fold a function over an iterator's lanes. Arguments are `[iter, body]`:
    /// the accumulator starts at lane 0 and, for each later lane, is replaced by
    /// `body` evaluated with `Arg(0)` bound to the accumulator and `Arg(1)` to the
    /// lane. The node's value is the final accumulator (e.g. a horizontal add).
    // #[arity = 2]
    Reduce,
    /// The k-th parameter of the innermost enclosing `Map`/`Reduce` lambda. A leaf
    /// carrying its index as an `Int` payload; only meaningful inside that lambda's
    /// `body` subexpression.
    Arg,
}

impl SymKind {
    /// Whether the operator is commutative in its two operands, so a builder may
    /// canonicalize operand order and the matcher may match either order.
    pub fn is_commutative(&self) -> bool {
        matches!(
            self,
            SymKind::Add | SymKind::Mul | SymKind::And | SymKind::Or | SymKind::Xor
        )
    }

    /// Number of operand children this node carries — its structural arity.
    pub fn arity(&self) -> usize {
        match self {
            SymKind::Symbol | SymKind::Constant | SymKind::Arg => 0,
            SymKind::Not
            | SymKind::Neg
            | SymKind::Log2Ceil
            | SymKind::Sqrt
            | SymKind::IterConcat => 1,
            SymKind::If
            | SymKind::Clamp
            | SymKind::Extract
            | SymKind::LoadMemory
            | SymKind::Fma => 3,
            SymKind::StoreMemory => 4,
            _ => 2,
        }
    }
}

/// The leaf/arity/operand-kind facts the program-DAG matcher needs. Structural
/// and context-independent, so the context type `C` is ignored — this lets the
/// same node label match in any context (e.g. an isel `tir::Context`).
impl<C> Matchable<C> for SymKind {
    fn is_leaf(&self, _: &C) -> bool {
        self.arity() == 0
    }

    fn num_children(&self, _: &C) -> usize {
        self.arity()
    }

    fn is_commutative(&self) -> bool {
        SymKind::is_commutative(self)
    }

    fn is_constant(&self) -> bool {
        matches!(self, SymKind::Constant)
    }
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
    /// Arbitrary-precision integers
    Int(APInt),
    /// Arbitrary-precision floats
    Float(APFloat),
    /// A fixed-size array of other values, like a vector
    Iterator(Vec<Value>),
    /// An untyped bag of bits
    RawBits(RawBits),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Iterator(a), Value::Iterator(b)) => a == b,
            _ => false,
        }
    }
}
