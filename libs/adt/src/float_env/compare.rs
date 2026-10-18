use std::cmp::Ordering;

use crate::{APFloat, Predicate};

use super::{
    FloatResult, FloatWidth,
    format::{Decoded, Format},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComparisonKind {
    Quiet,
    Signaling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatClass {
    SignalingNaN,
    QuietNaN,
    NegativeInfinity,
    NegativeNormal,
    NegativeSubnormal,
    NegativeZero,
    PositiveZero,
    PositiveSubnormal,
    PositiveNormal,
    PositiveInfinity,
}

pub fn compare_float(
    width: FloatWidth,
    lhs: u64,
    rhs: u64,
    predicate: Predicate,
    kind: ComparisonKind,
) -> FloatResult {
    let format = Format::new(width);
    let lhs_decoded = format.decode(lhs);
    let rhs_decoded = format.decode(rhs);
    let unordered =
        matches!(lhs_decoded, Decoded::Nan { .. }) || matches!(rhs_decoded, Decoded::Nan { .. });
    let signaling_nan = [lhs_decoded, rhs_decoded].iter().any(|value| {
        matches!(
            value,
            Decoded::Nan {
                signaling: true,
                ..
            }
        )
    });
    let relation = (!unordered)
        .then(|| value(width, lhs).compare(&value(width, rhs)))
        .flatten();
    let result = match predicate {
        Predicate::Oeq => relation == Some(Ordering::Equal),
        Predicate::Ogt => relation == Some(Ordering::Greater),
        Predicate::Oge => matches!(relation, Some(Ordering::Greater | Ordering::Equal)),
        Predicate::Olt => relation == Some(Ordering::Less),
        Predicate::Ole => matches!(relation, Some(Ordering::Less | Ordering::Equal)),
        Predicate::Une => relation != Some(Ordering::Equal),
        _ => false,
    };
    FloatResult {
        bits: u64::from(result),
        flags: u8::from(signaling_nan || kind == ComparisonKind::Signaling && unordered) << 4,
    }
}

pub fn classify_float(width: FloatWidth, bits: u64) -> FloatClass {
    let format = Format::new(width);
    match format.decode(bits) {
        Decoded::Nan {
            signaling: true, ..
        } => FloatClass::SignalingNaN,
        Decoded::Nan {
            signaling: false, ..
        } => FloatClass::QuietNaN,
        Decoded::Infinity(true) => FloatClass::NegativeInfinity,
        Decoded::Infinity(false) => FloatClass::PositiveInfinity,
        Decoded::Finite(value) if value.significand == 0 && value.negative => {
            FloatClass::NegativeZero
        }
        Decoded::Finite(value) if value.significand == 0 => FloatClass::PositiveZero,
        Decoded::Finite(value) if value.significand < 1 << format.fraction && value.negative => {
            FloatClass::NegativeSubnormal
        }
        Decoded::Finite(value) if value.significand < 1 << format.fraction => {
            FloatClass::PositiveSubnormal
        }
        Decoded::Finite(value) if value.negative => FloatClass::NegativeNormal,
        Decoded::Finite(_) => FloatClass::PositiveNormal,
    }
}

fn value(width: FloatWidth, bits: u64) -> APFloat {
    match width {
        FloatWidth::W32 => APFloat::from_bits(8, 23, false, bits as u128),
        FloatWidth::W64 => APFloat::from_bits(11, 52, false, bits as u128),
    }
}
