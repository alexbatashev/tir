//! The e-graph over the semantic vocabulary, and the readings every consumer of
//! it takes off an e-class.
//!
//! The label is [`SemNode`]; the classes are read the same way wherever the
//! e-graph is built — the selection axioms binding on class widths, the
//! peephole rules. What is read here is the
//! vocabulary's own business: a class's width, its integer binding, the IR type
//! a semantic type is spelled as. What a *backend* makes of a class — a
//! register's type, a low-bit view of one — stays with the backend.

use tir_egraph::{Carrier, Engine, Ref};

use crate::builtin::{FloatType, IntegerType};
use crate::sem::{FloatFormat, SemNode, SemPayload, SemType, SymKind, SymPayload};
use crate::vector::VectorType;
use crate::{Context, TypeId};
use tir_adt::APInt;

/// The semantic e-graph: e-classes of equivalent semantic expressions for the
/// values a region or a function computes.
pub type SemEGraph = Engine<SemNode>;

/// The constant a value is proven to hold: an offset of its carrier's zero,
/// the value an open assumption scope proves it evaluates to, or an integer
/// literal too wide for a carrier among its class's rows.
pub(crate) fn class_int_binding(egraph: &SemEGraph, value: Ref) -> Option<APInt> {
    let int = |n: &SemNode| match &n.payload {
        Some(SemPayload::Expr(SymPayload::Int(v))) => Some(v.clone()),
        _ => None,
    };
    let value = egraph.find(value);
    egraph.const_of(value).and_then(|n| int(&n)).or_else(|| {
        (value.offset == 0)
            .then(|| egraph.nodes(value.class).find_map(int))
            .flatten()
    })
}

/// An unsigned literal at its minimal width. Widths identify a constant class,
/// so the byte counts and metadata the memory vocabulary carries must be spelled
/// this one way wherever they are seeded.
pub(crate) fn minimal_unsigned_apint(value: u64) -> APInt {
    let width = if value == 0 {
        1
    } else {
        64 - value.leading_zeros()
    };
    APInt::new(width, value)
}

/// The negated comparison at the same operand order (`!(a < b)` is `a >= b`).
pub(crate) fn complement_comparison(kind: SymKind) -> Option<SymKind> {
    Some(match kind {
        SymKind::Eq => SymKind::Ne,
        SymKind::Ne => SymKind::Eq,
        SymKind::Lt => SymKind::Ge,
        SymKind::Ge => SymKind::Lt,
        SymKind::Gt => SymKind::Le,
        SymKind::Le => SymKind::Gt,
        SymKind::ULt => SymKind::UGe,
        SymKind::UGe => SymKind::ULt,
        SymKind::UGt => SymKind::ULe,
        SymKind::ULe => SymKind::UGt,
        _ => return None,
    })
}

/// Whether the kind is a boolean comparison.
pub(crate) fn is_comparison(kind: SymKind) -> bool {
    complement_comparison(kind).is_some()
}

/// The carrier a value of `ty` lives in: an integer of at most 64 bits, or a
/// pointer where a data layout gives its width. Anything else keeps no
/// offsets.
pub(crate) fn carrier_of(
    context: &Context,
    pointer_width: Option<u32>,
    ty: TypeId,
) -> Option<Carrier> {
    if context.is_state_type(ty) {
        return None;
    }
    let data = context.get_type_data(ty);
    let any = data.as_ref() as &dyn std::any::Any;
    if let Some(int) = any.downcast_ref::<IntegerType>() {
        return crate::sem::node::int_carrier(int.width());
    }
    any.downcast_ref::<crate::ptr::PtrType>()
        .and(pointer_width)
        .and_then(crate::sem::node::pointer_carrier)
}

// The width of a constant of the index type, which a length given to a vector
// operation has.
pub(crate) const LANES_WIDTH: u32 = 64;

// The count is held by the row as a child after its values.
pub(crate) fn counted_vector(context: &Context, ty: TypeId) -> Option<(TypeId, u32)> {
    let data = context.get_type_data(ty);
    let vector = (data.as_ref() as &dyn std::any::Any).downcast_ref::<VectorType>()?;
    let any = VectorType::dynamic(context, vector.element(context));
    Some((any, vector.length()?))
}

pub(crate) fn canonical_type(context: &Context, ty: TypeId) -> TypeId {
    let data = context.get_type_data(ty);
    if (data.as_ref() as &dyn std::any::Any).is::<crate::ptr::PtrType>() {
        return crate::ptr::PtrType::opaque(context);
    }
    counted_vector(context, ty).map_or(ty, |(any, _)| any)
}

/// The bit-width of an IR integer or float type, or `None` for any other type.
pub(crate) fn type_width(context: &Context, ty: TypeId) -> Option<u32> {
    if context.is_state_type(ty) {
        return None;
    }
    let data = context.get_type_data(ty);
    let any = data.as_ref() as &dyn std::any::Any;
    any.downcast_ref::<IntegerType>()
        .map(IntegerType::width)
        .or_else(|| any.downcast_ref::<FloatType>().map(FloatType::bit_width))
}

/// The context-independent semantic type represented by an IR type. Register
/// classes are intentionally absent: this describes the value, not its storage.
pub(crate) fn semantic_type(context: &Context, ty: TypeId) -> Option<SemType> {
    if context.is_state_type(ty) {
        return None;
    }
    let data = context.get_type_data(ty);
    let any = data.as_ref() as &dyn std::any::Any;
    any.downcast_ref::<IntegerType>()
        .map(|ty| SemType::bits(ty.width()))
        .or_else(|| {
            any.downcast_ref::<FloatType>()
                .map(|ty| SemType::Float(FloatFormat::new(ty.exp_width(), ty.mant_width())))
        })
        .or_else(|| {
            any.downcast_ref::<crate::fp::RoundingType>()
                .map(|_| SemType::bits(3))
        })
        .or_else(|| {
            any.downcast_ref::<crate::fp::EnvironmentType>()
                .map(|_| SemType::bits(13))
        })
        .or_else(|| {
            let elements = any
                .downcast_ref::<crate::builtin::TupleType>()?
                .elements(context);
            let [lhs, rhs] = elements.as_slice() else {
                return None;
            };
            Some(SemType::Pair(
                Box::new(semantic_type(context, *lhs)?),
                Box::new(semantic_type(context, *rhs)?),
            ))
        })
}

pub(crate) fn ir_type(context: &Context, ty: &SemType) -> Option<TypeId> {
    use crate::sem::Width;
    match ty {
        SemType::Bits(Width::Const(width)) | SemType::RawBits(Width::Const(width)) => {
            Some(IntegerType::new(context, *width))
        }
        SemType::Float(format) => match (&format.exponent, &format.mantissa) {
            (Width::Const(exponent), Width::Const(mantissa)) => {
                Some(FloatType::new(context, *exponent, *mantissa))
            }
            _ => None,
        },
        SemType::Pair(lhs, rhs) => Some(crate::builtin::TupleType::new(
            context,
            vec![ir_type(context, lhs)?, ir_type(context, rhs)?],
        )),
        SemType::Unit => Some(crate::builtin::UnitType::new(context)),
        _ => None,
    }
}
