//! Float fast-math flags and sem-derived float folding.

use tir::{
    attributes::AttributeValue,
    builtin::{fp_math_flags, ops, FastMathFlags, FloatType, UnitType, FPMATH_ATTR},
    func::ops as func_ops,
    sem::Value,
    ConstantFold, Context, Operation,
};
use tir_adt::APFloat;

#[test]
fn fp_ops_fold_via_sem() {
    let context = Context::with_default_dialects();
    let f32_ty = FloatType::f32(&context);
    let a = context.create_value(f32_ty, None);
    let b = context.create_value(f32_ty, None);
    let op = ops::mulf(&context, a.id(), b.id(), f32_ty).build();

    let fold = context
        .get_op(op.id())
        .as_interface::<dyn ConstantFold>()
        .expect("mulf derives ConstantFold from its sem");
    let folded = fold
        .fold(&[
            Value::Float(APFloat::from_f64(3.0)),
            Value::Float(APFloat::from_f64(0.5)),
        ])
        .expect("folds two constants");
    match folded {
        Value::Float(v) => assert_eq!(v.to_f64(), 1.5),
        other => panic!("expected a float, got {other:?}"),
    }
}

#[test]
fn fast_math_flags_parse_and_print() {
    assert_eq!(FastMathFlags::parse("none"), Some(FastMathFlags::NONE));
    assert_eq!(FastMathFlags::parse("fast"), Some(FastMathFlags::FAST));
    let flags = FastMathFlags::parse("contract, nnan").unwrap();
    assert!(flags.contains(FastMathFlags::CONTRACT));
    assert!(flags.contains(FastMathFlags::NNAN));
    assert!(!flags.contains(FastMathFlags::REASSOC));
    assert_eq!(flags.to_string(), "contract,nnan");
    assert_eq!(FastMathFlags::parse("wibble"), None);
    assert_eq!(FastMathFlags::FAST.to_string(), "fast");
    assert_eq!(
        FastMathFlags::parse(&FastMathFlags::FAST.to_string()),
        Some(FastMathFlags::FAST)
    );
}

/// A func with `fpmath` and an op in its body: the op inherits the func's
/// flags through the region chain; without any attribute the default is
/// strict.
#[test]
fn fp_math_flags_inherited_from_region_owner() {
    let context = Context::with_default_dialects();
    let f32_ty = FloatType::f32(&context);
    let unit = UnitType::new(&context);
    let func = func_ops::func(
        &context,
        "fma_candidate",
        unit,
        tir::builtin::FnType::new(&context, &[], unit),
        None,
    )
    .attr(FPMATH_ATTR, AttributeValue::Str("contract".into()))
    .build();
    let a = context.create_value(f32_ty, None);
    let b = context.create_value(f32_ty, None);
    let add = ops::addf(&context, a.id(), b.id(), f32_ty).build();
    func.body().insert(0, add.id());

    assert_eq!(fp_math_flags(&context, add.id()), FastMathFlags::CONTRACT);

    // Detached op: no enclosing scope, strict by default.
    let stray = ops::addf(&context, a.id(), b.id(), f32_ty).build();
    assert_eq!(fp_math_flags(&context, stray.id()), FastMathFlags::NONE);
}

/// A block-level `fpmath` shadows the flags the block would inherit, which is
/// what restores strictness inside a fast region.
#[test]
fn fp_math_flags_block_overrides_owner() {
    let context = Context::with_default_dialects();
    let f32_ty = FloatType::f32(&context);
    let unit = UnitType::new(&context);
    let func = func_ops::func(
        &context,
        "scoped",
        unit,
        tir::builtin::FnType::new(&context, &[], unit),
        None,
    )
    .attr(FPMATH_ATTR, AttributeValue::Str("fast".into()))
    .build();
    let a = context.create_value(f32_ty, None);
    let b = context.create_value(f32_ty, None);

    // An op one region deeper than the func that carries the attribute.
    let nested = context.create_region();
    let nested_block = context.create_block(vec![]);
    nested.add_block(nested_block.id());
    func.body()
        .append_op(func_ops::lambda(&context, "inner", unit, &nested).build());
    let add = ops::addf(&context, a.id(), b.id(), f32_ty).build();
    nested_block.append(add.id());

    // Before the override the inner block inherits `fast` through the nesting.
    assert_eq!(fp_math_flags(&context, add.id()), FastMathFlags::FAST);
    nested_block.set_attr(FPMATH_ATTR, AttributeValue::Str("none".into()));
    assert_eq!(fp_math_flags(&context, add.id()), FastMathFlags::NONE);
}
