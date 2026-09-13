mod arithmetic;
mod compare;
mod convert;
mod format;

pub use compare::{ComparisonKind, FloatClass, classify_float, compare_float};

use format::Format;

/// IEEE rounding directions and tie-breaking rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoundingMode {
    TiesToEven,
    TowardZero,
    TowardNegative,
    TowardPositive,
    TiesToAway,
}

/// IEEE floating-point operations with explicit rounding and exception flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Fma,
    Sqrt,
    Convert,
    SignedToFloat,
    UnsignedToFloat,
    FloatToSigned,
    FloatToUnsigned,
}

/// Binary floating-point or integer interchange width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatWidth {
    W32,
    W64,
}

impl FloatWidth {
    /// Number of bits in the interchange representation.
    pub const fn bit_width(self) -> u32 {
        match self {
            Self::W32 => 32,
            Self::W64 => 64,
        }
    }
}

/// Result bits and IEEE flags ordered as invalid, divide-by-zero, overflow,
/// underflow, and inexact in bits four through zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatResult {
    pub bits: u64,
    pub flags: u8,
}

/// Evaluate raw bits with one rounding. Widths describe the integer side for
/// integer conversions and IEEE binary formats otherwise. Arithmetic uses the
/// source format. Unused operands are ignored. NaNs retain the first NaN
/// operand's sign and payload, with signaling NaNs quieted. Integer results
/// contain only destination-width bits and are unspecified on invalid input.
/// Tininess is tested after rounding to destination precision with an
/// unbounded exponent, before encoding subnormal results.
pub fn eval_float(
    op: FloatOp,
    source: FloatWidth,
    destination: FloatWidth,
    operands: [u64; 3],
    rounding: RoundingMode,
) -> FloatResult {
    match op {
        FloatOp::SignedToFloat | FloatOp::UnsignedToFloat => convert::from_integer(
            operands[0],
            source,
            Format::new(destination),
            op == FloatOp::SignedToFloat,
            rounding,
        ),
        FloatOp::FloatToSigned | FloatOp::FloatToUnsigned => convert::to_integer(
            operands[0],
            Format::new(source),
            destination,
            op == FloatOp::FloatToSigned,
            rounding,
        ),
        FloatOp::Convert => convert::float(
            operands[0],
            Format::new(source),
            Format::new(destination),
            rounding,
        ),
        FloatOp::Add
        | FloatOp::Sub
        | FloatOp::Mul
        | FloatOp::Div
        | FloatOp::Fma
        | FloatOp::Sqrt => arithmetic::eval(op, Format::new(source), operands, rounding),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod differential;

#[cfg(test)]
mod edge_cases;
