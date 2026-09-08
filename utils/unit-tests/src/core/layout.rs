//! Data layout, target environment and scoped-attribute resolution.

use tir::{
    attributes::AttributeValue,
    builtin::{ops, FloatType, IntegerType, ModuleOp, UnitType},
    func::ops as func_ops,
    parse::ir::parse_ir,
    ptr::PtrType,
    scoped_dict, Context, DataLayout, Endianness, Operation, TargetEnv,
};

use super::fixtures;

/// The layout of a module declaring `spec`.
fn layout(context: &Context, spec: &str) -> DataLayout {
    let src = format!("module {{data_layout = {spec}}} {{\n  module_end\n}}");
    let module = parse_ir::<ModuleOp>(context, &src).expect("parse module");
    DataLayout::for_op(context, module.id()).expect("layout in scope")
}

/// Every data-layout accessor reads its entry out of the spec the module
/// declares, and answers "absent" for what the spec leaves out.
#[test]
fn layout_accessors_read_the_declared_spec() {
    type Probe = fn(&Context, &DataLayout);

    let cases: &[(&str, Probe)] = &[
        (r#"{endianness = "little"}"#, |_, layout| {
            assert_eq!(layout.endianness(), Some(Endianness::Little));
        }),
        (r#"{endianness = "big"}"#, |_, layout| {
            assert_eq!(layout.endianness(), Some(Endianness::Big));
        }),
        ("{stack_alignment = 128}", |_, layout| {
            assert_eq!(layout.stack_alignment(), Some(128));
        }),
        ("{types = {p = {size = 32, abi = 32}}}", |_, layout| {
            assert_eq!(layout.pointer_size(), Some(32));
        }),
        // A class layout is read without a type of that class existing.
        ("{types = {f64 = {size = 64, abi = 32}}}", |_, layout| {
            assert_eq!(layout.class_layout("f64"), Some((64, 32)));
            assert_eq!(layout.class_layout("f32"), None);
        }),
        // An integer's size defaults to its own width; a declared one wins.
        ("{types = {i32 = {abi = 32}}}", |context, layout| {
            let i32_ty = IntegerType::new(context, 32);
            assert_eq!(layout.size_in_bits(context, i32_ty), Some(32));
        }),
        ("{types = {i1 = {size = 8, abi = 8}}}", |context, layout| {
            let i1 = IntegerType::new(context, 1);
            assert_eq!(layout.size_in_bits(context, i1), Some(8));
        }),
        // ABI alignment is read per type class...
        (
            "{types = {i16 = {abi = 16}, f64 = {abi = 64}, p = {size = 64, abi = 64}}}",
            |context, layout| {
                let i16_ty = IntegerType::new(context, 16);
                let f64_ty = FloatType::f64(context);
                let pointer = PtrType::opaque(context);
                assert_eq!(layout.abi_alignment(context, i16_ty), Some(16));
                assert_eq!(layout.abi_alignment(context, f64_ty), Some(64));
                assert_eq!(layout.abi_alignment(context, pointer), Some(64));
            },
        ),
        // ... and an undeclared class has none.
        ("{types = {i64 = {abi = 64}}}", |context, layout| {
            let i128_ty = IntegerType::new(context, 128);
            assert_eq!(layout.abi_alignment(context, i128_ty), None);
        }),
        // Preferred alignment defaults to the ABI one, and is read when given.
        ("{types = {i16 = {abi = 16}}}", |context, layout| {
            let i16_ty = IntegerType::new(context, 16);
            assert_eq!(layout.preferred_alignment(context, i16_ty), Some(16));
        }),
        (
            "{types = {i16 = {abi = 16, preferred = 32}}}",
            |context, layout| {
                let i16_ty = IntegerType::new(context, 16);
                assert_eq!(layout.preferred_alignment(context, i16_ty), Some(32));
            },
        ),
        // Entries outside the predefined set stay readable.
        ("{address_spaces = {global = 1}}", |_, layout| {
            assert!(layout.get("address_spaces").is_some());
            assert!(layout.get("endianness").is_none());
        }),
    ];

    for (spec, probe) in cases {
        let context = Context::with_default_dialects();
        probe(&context, &layout(&context, spec));
    }
}

#[test]
fn ir_entries_override_the_target_default_key_by_key() {
    let default = tir::data_layout_spec(Endianness::Big, 128, &[("i32", 32, 32), ("p", 64, 64)]);
    let (context, module) = fixtures::parse(
        r#"module {data_layout = {endianness = "little", types = {p = {size = 32, abi = 32}}}} {
  module_end
}"#,
    );

    let layout = DataLayout::for_op_with_default(&context, module.id(), Some(&default))
        .expect("target default applies");

    // The module overrides byte order and the pointer entry; the i32 entry
    // and the stack alignment it never mentions come from the target.
    assert_eq!(layout.endianness(), Some(Endianness::Little));
    assert_eq!(layout.pointer_size(), Some(32));
    assert_eq!(
        layout.abi_alignment(&context, IntegerType::new(&context, 32)),
        Some(32)
    );
    assert_eq!(layout.stack_alignment(), Some(128));
}

#[test]
fn the_target_default_applies_where_the_ir_declares_nothing() {
    let default = tir::data_layout_spec(Endianness::Little, 64, &[("p", 64, 64)]);
    let (context, module) = fixtures::parse("module {\n  module_end\n}");

    let layout = DataLayout::for_op_with_default(&context, module.id(), Some(&default))
        .expect("target default applies");

    assert_eq!(layout.pointer_size(), Some(64));
}

#[test]
fn a_target_default_needs_no_enclosing_scope() {
    let spec = AttributeValue::Dict(Box::new(
        [("stack_alignment".to_string(), AttributeValue::UInt(64))]
            .into_iter()
            .collect(),
    ));

    let layout = DataLayout::from_value(&spec).expect("spec is a dict");

    assert_eq!(layout.stack_alignment(), Some(64));
    assert!(DataLayout::from_value(&AttributeValue::UInt(64)).is_none());
}

/// The environment of a module declaring `spec`.
fn target_env(context: &Context, spec: &str) -> TargetEnv {
    let src = format!("module {{target_env = {spec}}} {{\n  module_end\n}}");
    let module = parse_ir::<ModuleOp>(context, &src).expect("parse module");
    TargetEnv::for_op(context, module.id()).expect("environment in scope")
}

/// Every target-environment accessor reads its entry out of the spec the
/// module declares, and answers "absent" for what the spec leaves out.
#[test]
fn target_env_accessors_read_the_declared_spec() {
    type Probe = fn(&TargetEnv);

    let cases: &[(&str, Probe)] = &[
        (r#"{arch = "riscv64", cpu = "sifive-u74"}"#, |env| {
            assert_eq!(env.arch(), Some("riscv64"));
            assert_eq!(env.cpu(), Some("sifive-u74"));
        }),
        (r#"{arch = "arm64"}"#, |env| assert_eq!(env.cpu(), None)),
        (r#"{arch = "riscv64", features = ["m", "c"]}"#, |env| {
            assert!(env.has_feature("m"));
            assert!(env.has_feature("c"));
            assert!(!env.has_feature("v"));
        }),
        (r#"{arch = "riscv64"}"#, |env| {
            assert!(!env.has_feature("m"));
        }),
        // Entries outside the predefined set stay readable.
        ("{shared_memory = 65536}", |env| {
            assert_eq!(env.get("shared_memory"), Some(&AttributeValue::Int(65536)));
        }),
    ];

    for (spec, probe) in cases {
        let context = Context::with_default_dialects();
        probe(&target_env(&context, spec));
    }
}

#[test]
fn a_target_description_needs_no_enclosing_scope() {
    let spec = AttributeValue::Dict(Box::new(
        [(
            "arch".to_string(),
            AttributeValue::Str("arm64".to_string().into()),
        )]
        .into_iter()
        .collect(),
    ));

    let env = TargetEnv::from_value(&spec).expect("spec is a dict");

    assert_eq!(env.arch(), Some("arm64"));
    assert!(TargetEnv::from_value(&AttributeValue::UInt(0)).is_none());
}

fn dict(entries: impl IntoIterator<Item = (&'static str, AttributeValue)>) -> AttributeValue {
    AttributeValue::Dict(Box::new(
        entries
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    ))
}

#[test]
fn nothing_is_in_scope_without_the_attribute() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    assert!(scoped_dict(&context, module.id(), "data_layout").is_none());
}

#[test]
fn a_nested_op_reads_the_enclosing_scope() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None)
        .attr("data_layout", dict([("endianness", "little".into())]))
        .build();
    let nested = module.body().append_op(ops::module_end(&context).build());

    let resolved = scoped_dict(&context, nested.id(), "data_layout").expect("module scope");

    assert_eq!(resolved.get("endianness"), Some(&"little".into()));
}

#[test]
fn an_inner_scope_overrides_one_nested_entry() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None)
        .attr(
            "data_layout",
            dict([
                ("endianness", "little".into()),
                (
                    "types",
                    dict([
                        ("i32", dict([("abi", 32.into())])),
                        ("i64", dict([("abi", 64.into())])),
                    ]),
                ),
            ]),
        )
        .build();
    let func = module.body().append_op(
        func_ops::func(
            &context,
            "f",
            UnitType::new(&context),
            tir::builtin::FnType::new(&context, &[], UnitType::new(&context)),
            None,
        )
        .attr(
            "data_layout",
            dict([("types", dict([("i32", dict([("abi", 8.into())]))]))]),
        )
        .build(),
    );

    let resolved = scoped_dict(&context, func.id(), "data_layout").expect("module scope");

    let AttributeValue::Dict(types) = &resolved["types"] else {
        panic!("types must stay a dict");
    };
    // The func overrides i32 only: i64 and the sibling endianness survive.
    assert_eq!(types["i32"], dict([("abi", 8.into())]));
    assert_eq!(types["i64"], dict([("abi", 64.into())]));
    assert_eq!(resolved.get("endianness"), Some(&"little".into()));
}

#[test]
fn an_inner_scope_replaces_an_array_entry() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None)
        .attr(
            "target_env",
            dict([("features", vec!["m".into(), "a".into()].into())]),
        )
        .build();
    let nested = module.body().append_op(
        ops::module_end(&context)
            .attr("target_env", dict([("features", vec!["c".into()].into())]))
            .build(),
    );

    let resolved = scoped_dict(&context, nested.id(), "target_env").expect("module scope");

    assert_eq!(resolved["features"], vec!["c".into()].into());
}
