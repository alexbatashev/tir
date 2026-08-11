//! The e-graph node label for semantic instruction selection, plus the small
//! shared helpers that read types and operand bindings off e-classes.
//!
//! The label itself is [`tir::sem::SemNode`] — the one vocabulary the sea view,
//! the peephole and selection all speak.

use std::collections::HashMap;

use tir::{
    Context, TypeId, ValueId,
    builtin::{FloatType, IntegerType},
    sem::{FloatFormat, SemType, SymKind, SymPayload},
};
use tir_adt::APInt;
use tir_symbolic::egraph::{EGraph, ENode, Id};

pub(crate) use tir::sem::template_node;
pub use tir::sem::{SemNode, SemPayload};

/// The semantic e-graph instruction selection operates over: e-classes of
/// equivalent semantic expressions for the values computed across the whole
/// function, shared by every block and covered per block inside per-block
/// assumption scopes.
pub type SemEGraph = EGraph<SemNode>;

/// If the class is a low-bit truncation `Extract(v, hi, 0)`, its operand class
/// `v`. Such a value *is* the low `hi+1` bits of `v`'s register — the framework's
/// value model (a width-n value occupies the low n bits, upper bits undefined) —
/// so it computes nothing: consumers read `v`'s register directly. No
/// materializer, no instruction, and no cross-width union (the i32 view and any
/// explicit i64 widening stay distinct classes, kept apart by the width matcher).
pub(crate) fn low_extract_source(egraph: &SemEGraph, class: Id) -> Option<Id> {
    egraph.nodes(class).iter().find_map(|n| {
        (n.kind == SymKind::Extract
            && n.children().len() == 3
            && class_int_binding(egraph, egraph.find(n.children()[2]))
                .as_ref()
                .map(APInt::to_u64)
                == Some(0))
        .then(|| egraph.find(n.children()[0]))
    })
}

/// Whether the class is a low-bit truncation (see [`low_extract_source`]).
pub(crate) fn is_low_extract_view(egraph: &SemEGraph, class: Id) -> bool {
    low_extract_source(egraph, class).is_some()
}

pub(crate) fn low_extract_width(egraph: &SemEGraph, class: Id) -> Option<u32> {
    egraph.nodes(class).iter().find_map(|node| {
        if node.kind != SymKind::Extract || node.children().len() != 3 {
            return None;
        }
        let hi = class_int_binding(egraph, egraph.find(node.children()[1]))?.to_u64();
        let lo = class_int_binding(egraph, egraph.find(node.children()[2]))?.to_u64();
        (lo == 0).then(|| u32::try_from(hi + 1).ok()).flatten()
    })
}

/// The constant a class is proven to hold, if any member is an integer literal.
pub(crate) fn class_int_binding(egraph: &SemEGraph, class: Id) -> Option<APInt> {
    egraph.nodes(class).iter().find_map(|n| match &n.payload {
        Some(SemPayload::Expr(SymPayload::Int(v))) => Some(v.clone()),
        _ => None,
    })
}

/// The register value carrying a class: an input value, then the first IR value
/// the class computes (from `class_values`, the map recording which values a
/// class stands for). The representative feeds cost-model approximation only.
pub(crate) fn class_value_binding(
    egraph: &SemEGraph,
    class_values: &HashMap<Id, Vec<ValueId>>,
    class: Id,
) -> Option<ValueId> {
    egraph
        .nodes(class)
        .iter()
        .find_map(|n| match n.payload.as_ref() {
            Some(SemPayload::Expr(SymPayload::Value(v))) => Some(*v),
            _ => None,
        })
        .or_else(|| {
            class_values
                .get(&egraph.find(class))
                .and_then(|values| values.first().copied())
        })
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

/// The bit-width of an IR integer or float type, or `None` for any other type.
pub(crate) fn type_width(context: &Context, ty: TypeId) -> Option<u32> {
    let data = context.get_type_data(ty);
    let any = data.as_ref() as &dyn std::any::Any;
    any.downcast_ref::<IntegerType>()
        .map(IntegerType::width)
        .or_else(|| {
            any.downcast_ref::<tir::builtin::FloatType>()
                .map(tir::builtin::FloatType::bit_width)
        })
}

/// The context-independent semantic type represented by an IR type. Register
/// classes are intentionally absent: this describes the value, not its storage.
pub(crate) fn semantic_type(context: &Context, ty: TypeId) -> Option<SemType> {
    let data = context.get_type_data(ty);
    let any = data.as_ref() as &dyn std::any::Any;
    any.downcast_ref::<IntegerType>()
        .map(|ty| SemType::bits(ty.width()))
        .or_else(|| {
            any.downcast_ref::<FloatType>()
                .map(|ty| SemType::Float(FloatFormat::new(ty.exp_width(), ty.mant_width())))
        })
}

pub(crate) fn ir_type(context: &Context, ty: &SemType) -> Option<TypeId> {
    use tir::sem::Width;
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
        _ => None,
    }
}

pub(crate) fn minimal_unsigned_apint(value: u64) -> APInt {
    let width = if value == 0 {
        1
    } else {
        64 - value.leading_zeros()
    };
    APInt::new(width, value)
}

/// Whether duplicating the class's computation is sound: every member is a pure
/// value expression, so two fused matches may each recompute it inside their
/// instruction. Memory effects are excluded — two reads of the same address are
/// not interchangeable across an intervening write.
pub(crate) fn class_is_pure(egraph: &SemEGraph, class: Id) -> bool {
    egraph
        .nodes(class)
        .iter()
        .all(|n| n.sym().is_some_and(kind_is_pure))
}

/// Whether the kind is a pure value expression (see [`class_is_pure`]).
pub(crate) fn kind_is_pure(kind: SymKind) -> bool {
    !matches!(
        kind,
        SymKind::LoadMemory
            | SymKind::StoreMemory
            | SymKind::LoadReserved
            | SymKind::StoreConditional
            | SymKind::AtomicRmw
            | SymKind::Fence
    )
}

/// The integer width of an e-class, taken from whichever member carries a known
/// integer type.
pub(crate) fn class_width(ctx: &Context, egraph: &SemEGraph, class: Id) -> Option<u32> {
    egraph
        .nodes(class)
        .iter()
        .find_map(|n| n.ty.and_then(|ty| type_width(ctx, ty)))
}

/// A ground semantic type carried by any typed member of an e-class.
pub(crate) fn class_semantic_type(ctx: &Context, egraph: &SemEGraph, class: Id) -> Option<SemType> {
    egraph
        .nodes(class)
        .iter()
        .find_map(|node| node.ty.and_then(|ty| semantic_type(ctx, ty)))
}

/// The semantic type a register must hold for an e-class. Pointers preserve
/// their IR type in the graph, but use the target data layout's pointer width at
/// an instruction's register boundary.
pub(crate) fn class_register_type(
    ctx: &Context,
    egraph: &SemEGraph,
    class: Id,
    pointer_width: Option<u32>,
) -> Option<SemType> {
    class_semantic_type(ctx, egraph, class).or_else(|| {
        let width = pointer_width?;
        egraph
            .nodes(class)
            .iter()
            .any(|node| {
                node.ty.is_some_and(|ty| {
                    let data = ctx.get_type_data(ty);
                    (data.as_ref() as &dyn std::any::Any)
                        .downcast_ref::<tir::ptr::PtrType>()
                        .is_some()
                })
            })
            .then(|| SemType::bits(width))
    })
}
