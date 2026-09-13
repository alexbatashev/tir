//! Unit tests for the `tir-riscv` backend's public API.

use tir::backend::isel::prove_guarded_relaxations;
use tir::backend::TargetMachine;
use tir::graph::{Dag, MutDag};
use tir::sem::{FloatFormat, SemType, SymKind, SymPayload};
use tir::Context;
use tir_riscv::{Feature, RegClass, TargetConfig};

fn target(march: &str) -> Box<dyn TargetMachine> {
    tir::backend::select_target(march, None, None).expect("march should select")
}

fn info(name: &str) -> &'static tir::backend::InstrInfo {
    super::support::info("riscv", tir_riscv::instruction_infos(), name)
}

#[test]
fn target_abi_matches_lp64d_register_convention() {
    let target =
        tir::backend::select_target_with_abi("riscv64", None, None, Some("lp64d")).unwrap();
    let abi = target.abi();
    let args = |kind| {
        abi.args
            .iter()
            .find(|sequence| sequence.kind == kind)
            .unwrap()
    };
    let rets = |kind| {
        abi.rets
            .iter()
            .find(|sequence| sequence.kind == kind)
            .unwrap()
    };

    assert_eq!(abi.name, "lp64d");
    assert_eq!(abi.sp, (RegClass::GPR.id(), 2));
    assert_eq!(abi.ra, Some((RegClass::GPR.id(), 1)));
    assert_eq!(abi.fp, Some((RegClass::GPR.id(), 8)));
    assert_eq!(abi.indirect_result, Some((RegClass::GPR.id(), 10)));
    assert_eq!(abi.stack.align, 16);
    assert_eq!(abi.stack.slot_size, 8);
    assert_eq!(
        args(tir::backend::abi::ValueKind::Int)
            .regs
            .iter()
            .map(|register| register.1)
            .collect::<Vec<_>>(),
        (10..=17).collect::<Vec<_>>()
    );
    assert_eq!(
        args(tir::backend::abi::ValueKind::Float)
            .regs
            .iter()
            .map(|register| register.1)
            .collect::<Vec<_>>(),
        (10..=17).collect::<Vec<_>>()
    );
    assert_eq!(
        rets(tir::backend::abi::ValueKind::Int)
            .regs
            .iter()
            .map(|register| register.1)
            .collect::<Vec<_>>(),
        vec![10, 11]
    );
    assert_eq!(
        rets(tir::backend::abi::ValueKind::Float)
            .regs
            .iter()
            .map(|register| register.1)
            .collect::<Vec<_>>(),
        vec![10, 11]
    );
    assert!(abi.callee_saved.contains(&(RegClass::GPR.id(), 8)));
    assert!(abi.callee_saved.contains(&(RegClass::FPR64.id(), 8)));
}

#[test]
fn target_selection_accepts_and_validates_mabi() {
    let target =
        tir::backend::select_target_with_abi("riscv64", None, None, Some("lp64d")).unwrap();
    assert_eq!(target.abi().name, "lp64d");

    let target = tir::backend::select_target("rv64i", None, None).unwrap();
    assert_eq!(target.abi().name, "lp64");

    let target = tir::backend::select_target("rv64id", None, None).unwrap();
    assert_eq!(target.abi().name, "lp64d");

    let target = tir::backend::select_target("rv64if", None, None).unwrap();
    assert_eq!(target.abi().name, "lp64f");

    let error = tir::backend::select_target_with_abi("riscv64", None, None, Some("unknown"))
        .err()
        .unwrap();
    assert_eq!(
        error,
        "unknown ABI 'unknown' for riscv (available: lp64, lp64f, lp64d)"
    );
}

#[test]
fn memory_and_patch_facts_are_per_opcode() {
    // A load's behavior reads memory and its branch-offset immediate is
    // patchable once layout is known; `add` has no immediate to patch.
    assert!(info("lw").effects.reads);
    assert!(info("sw").effects.writes);
    let patch_fields =
        |info: &tir::backend::InstrInfo| info.encode.expect("encodes").shapes[0].patch.len();
    assert_eq!(patch_fields(info("beq")), 1);
    assert_eq!(patch_fields(info("add")), 0);
}

#[test]
fn machine_models_resolve_scheduling_classes() {
    // ALU ops resolve to the ALU unit (via the WriteIALU schedule on their
    // template), loads/stores to the LSU, and an instruction with no schedule
    // class (e.g. the M-extension `mul`, unmodeled here) falls back to default.
    for model in [
        tir_riscv::in_order_core_model(),
        tir_riscv::out_of_order_core_model(),
    ] {
        assert_eq!(info("add").sched_on(&model).resources, &["ALU"]);
        assert_eq!(info("sub").sched_on(&model).resources, &["ALU"]);
        assert_eq!(info("lw").sched_on(&model).resources, &["LSU"]);
        assert_eq!(info("sw").sched_on(&model).resources, &["LSU"]);
        assert_eq!(
            info("mul").sched_on(&model),
            tir::backend::sched::InstrSchedClass::DEFAULT
        );
    }
}

#[test]
fn phase_based_timing_resolves_from_pipeline() {
    // InOrderCore is phase-based: a 5-stage pipeline (IF ID EX MEM WB), operands
    // read at ID (cycle 1), results written at EX/MEM.
    let in_order = tir_riscv::in_order_core_model();
    assert_eq!(in_order.phase_cycle("ID"), Some(1));
    assert_eq!(in_order.phase_cycle("MEM"), Some(3));

    // add: read@ID(1) → write@EX(2) ⇒ latency 1, read_cycle 1, write_cycle 2.
    let add = info("add").sched_on(&in_order);
    assert_eq!((add.read_cycle, add.latency, add.write_cycle()), (1, 1, 2));
    // lw: read@ID(1) → write@MEM(3) ⇒ latency 2, read_cycle 1, write_cycle 3.
    let lw = info("lw").sched_on(&in_order);
    assert_eq!((lw.read_cycle, lw.latency, lw.write_cycle()), (1, 2, 3));

    // OutOfOrderCore is scalar (`latency = N`): read at cycle 0, no pipeline.
    let ooo = tir_riscv::out_of_order_core_model();
    assert!(ooo.pipeline.is_empty());
    let ooo_lw = info("lw").sched_on(&ooo);
    assert_eq!((ooo_lw.read_cycle, ooo_lw.latency), (0, 4));
}

#[test]
fn instruction_cost_reflects_unit_defaults() {
    // Machine-independent cost comes from the `unit` defaults, not a machine's
    // `bind`: WriteIALU defaults latency 1, WriteLoad defaults latency 3.
    assert_eq!(info("add").cost, 1);
    assert_eq!(info("lw").cost, 3);
    // Instructions with no `schedule` block fall back to the default cost.
    assert_eq!(info("sub").cost, 1);
    assert_eq!(info("mul").cost, 1);

    // The per-machine model may refine the generic default for that silicon:
    // both demo cores bind WriteLoad to latency 4, independent of the default 3.
    assert_eq!(
        info("lw")
            .sched_on(&tir_riscv::out_of_order_core_model())
            .latency,
        4
    );
}

#[test]
fn override_supersedes_unit_bind() {
    // OutOfOrderCore overrides `Add` to latency 2, beating WriteIALU's bind (1).
    assert_eq!(
        info("add")
            .sched_on(&tir_riscv::out_of_order_core_model())
            .latency,
        2
    );
    // InOrderCore has no override → `add` resolves from its WriteIALU bind.
    assert_eq!(
        info("add")
            .sched_on(&tir_riscv::in_order_core_model())
            .latency,
        1
    );
}

#[test]
fn forwarding_paths_are_modeled() {
    let in_order = tir_riscv::in_order_core_model();
    assert_eq!(in_order.forward_latency("ALU", "ALU"), Some(0));
    assert_eq!(in_order.forward_latency("LSU", "ALU"), Some(1));
    assert_eq!(in_order.forward_latency("ALU", "LSU"), None);
    // OutOfOrderCore declares no forwarding network.
    assert!(tir_riscv::out_of_order_core_model().forwards.is_empty());
}

#[test]
fn in_order_and_ooo_differ_structurally() {
    let in_order = tir_riscv::in_order_core_model();
    assert_eq!(in_order.name, "InOrderCore");
    assert_eq!(in_order.issue_width, 1);
    assert_eq!(in_order.buffer("rob"), None); // no reorder buffer
    assert_eq!(in_order.resource("ALU").map(|r| r.units), Some(1));

    let ooo = tir_riscv::out_of_order_core_model();
    assert_eq!(ooo.name, "OutOfOrderCore");
    assert_eq!(ooo.issue_width, 4);
    assert_eq!(ooo.buffer("rob"), Some(128));
    assert_eq!(ooo.resource("ALU").map(|r| r.units), Some(4));
}

#[test]
fn machines_filter_by_feature_set() {
    let rv64 = target("rv64im");
    assert_eq!(rv64.machines(), vec!["rv64-in-order", "rv64-ooo"]);
    assert!(rv64.machine_model("rv64-ooo").is_some());
    assert!(rv64.machine_model("scr1-3stage").is_none());

    let rv32 = target("rv32i");
    assert_eq!(rv32.machines(), vec!["scr1-3stage"]);
    assert!(rv32.machine_model("scr1-3stage").is_some());
    assert!(rv32.machine_model("rv64-ooo").is_none());
}

#[test]
fn isel_rules_filter_by_feature_set() {
    let context = Context::with_default_dialects();
    let rule_names = |features: &[Feature]| -> Vec<&'static str> {
        tir_riscv::get_isel_rules(&context, features)
            .iter()
            .map(|r| r.name)
            .collect()
    };

    let rv64i = rule_names(&[Feature::RV64I]);
    assert!(rv64i.contains(&"addword"));
    assert!(!rv64i.contains(&"mul"));

    let rv64im = rule_names(&[Feature::RV64I, Feature::RVM]);
    assert!(rv64im.contains(&"mul"));

    let rv32i = rule_names(&[Feature::RV32I]);
    assert!(rv32i.contains(&"add"));
    assert!(!rv32i.contains(&"addword"));
    assert!(!rv32i.contains(&"loaddoubleword"));

    // F gates the single-precision rules, D the double-precision ones.
    assert!(!rv32i.contains(&"faddsrne"));
    let rv32if = rule_names(&[Feature::RV32I, Feature::F]);
    assert!(rv32if.contains(&"faddsrne"));
    assert!(rv32if.contains(&"floadword"));
    assert!(rv32if.contains(&"fmvwx"));
    assert!(!rv32if.contains(&"fadddrne"));
    let rv64ifd = rule_names(&[Feature::RV64I, Feature::F, Feature::D, Feature::D64]);
    assert!(rv64ifd.contains(&"faddsrne"));
    assert!(rv64ifd.contains(&"fadddrne"));
    assert!(rv64ifd.contains(&"fmvdx"));
    assert!(rv64ifd.contains(&"fstoredouble"));
}

#[test]
fn state_preserving_float_sequences_prove_complete_behavior() {
    let context = Context::with_default_dialects();
    for march in ["rv32ifd", "rv64ifd"] {
        let config = TargetConfig::parse(march, None, None).unwrap();
        let rules: Vec<_> = tir_riscv::get_isel_rules(&context, config.features())
            .into_iter()
            .filter(|rule| rule.steps.len() == 3)
            .collect();

        assert!(!rules.is_empty(), "{march}");
        prove_guarded_relaxations(&rules).unwrap();
    }
}

#[test]
fn directed_float_rule_keeps_its_rounding_mode() {
    let context = Context::with_default_dialects();
    let rules = tir_riscv::get_isel_rules(
        &context,
        &[Feature::RV64I, Feature::F, Feature::F64, Feature::D],
    );
    let rule = rules.iter().find(|rule| rule.name == "fadddrup").unwrap();
    let event = rule.pattern.root().unwrap();
    assert_eq!(*rule.pattern.get_kind(event), SymKind::StateResult);
    let value = rule.pattern.children(event).next().unwrap();
    assert_eq!(*rule.pattern.get_kind(value), SymKind::FPValue);
    let outcome = rule.pattern.children(value).next().unwrap();
    assert_eq!(*rule.pattern.get_kind(outcome), SymKind::FAddRound);
    let rounding = rule.pattern.children(outcome).last().unwrap();
    assert!(matches!(
        rule.pattern.get_leaf_data(rounding),
        Some(SymPayload::Int(value)) if value.width() == 3 && value.to_u64() == 3
    ));
}

#[test]
fn float_comparison_rule_infers_contextual_literal_widths() {
    let context = Context::with_default_dialects();
    let rules = tir_riscv::get_isel_rules(
        &context,
        &[Feature::RV64I, Feature::F, Feature::F64, Feature::D],
    );
    let rule = rules.iter().find(|rule| rule.name == "feqd").unwrap();

    let result = tir::sem::infer_types(&rule.pattern, |_| None);
    assert!(
        result.is_ok(),
        "feqd pattern type inference failed: {result:?}"
    );
}

#[test]
fn double_comparison_rules_accept_float_register_operands() {
    let context = Context::with_default_dialects();
    let rules = tir_riscv::get_isel_rules(
        &context,
        &[Feature::RV64I, Feature::F, Feature::F64, Feature::D],
    );
    let f64 = SemType::Float(FloatFormat::new(11, 52));

    for name in ["feqd", "fltd", "fled"] {
        let rule = rules.iter().find(|rule| rule.name == name).unwrap();
        let result = tir::sem::infer_types(&rule.pattern, |node| {
            let Some(SymPayload::SymbolId(symbol)) = rule.pattern.get_leaf_data(node) else {
                return None;
            };
            let requirement = rule
                .operand_registers
                .iter()
                .find_map(|(operand, requirement)| (operand == symbol).then_some(requirement))?;
            if requirement.accepts(&f64) && !requirement.accepts(&SemType::bits(64)) {
                Some(f64.clone())
            } else {
                Some(SemType::bits(requirement.width()))
            }
        });
        assert!(
            result.is_ok(),
            "{name} pattern rejects its declared register operand types: {result:?}"
        );
    }
}

#[test]
fn dynamic_float_rule_derives_rounding_and_flag_effects() {
    let context = Context::with_default_dialects();
    let rules = tir_riscv::get_isel_rules(
        &context,
        &[Feature::RV64I, Feature::F, Feature::F64, Feature::D],
    );
    let rule = rules.iter().find(|rule| rule.name == "faddd").unwrap();
    let kinds: Vec<_> = rule
        .pattern
        .preorder(rule.pattern.root().unwrap())
        .map(|node| *rule.pattern.get_kind(node))
        .collect();
    assert!(kinds.contains(&SymKind::StateResult));
    assert!(kinds.contains(&SymKind::StateAssign));
    assert!(!kinds.contains(&SymKind::StateIf));
    assert!(!kinds.contains(&SymKind::StateTrap));
    assert!(
        kinds
            .iter()
            .filter(|kind| **kind == SymKind::StateRead)
            .count()
            >= 2
    );
    let guarded = rule
        .guarded_semantics
        .as_ref()
        .expect("dynamic rounding keeps its invalid-mode behavior");
    let guarded_kinds: Vec<_> = guarded
        .preorder(guarded.root().unwrap())
        .map(|node| *guarded.get_kind(node))
        .collect();
    assert!(guarded_kinds.contains(&SymKind::StateIf));
    assert!(guarded_kinds.contains(&SymKind::StateTrap));
    let fallback =
        tir_symbolic::lang::selection_fallback(guarded, guarded.root().unwrap()).unwrap();
    let fallback_kinds: Vec<_> = fallback
        .preorder(fallback.root().unwrap())
        .map(|node| *fallback.get_kind(node))
        .collect();
    assert!(!fallback_kinds.contains(&SymKind::StateIf));
    assert!(!fallback_kinds.contains(&SymKind::StateTrap));
    prove_guarded_relaxations(std::slice::from_ref(rule)).unwrap();
}

#[test]
fn dynamic_float_rule_rejects_guard_that_traps_valid_rounding_mode() {
    let context = Context::with_default_dialects();
    let mut rules = tir_riscv::get_isel_rules(
        &context,
        &[Feature::RV64I, Feature::F, Feature::F64, Feature::D],
    );
    let rule = rules.iter_mut().find(|rule| rule.name == "faddd").unwrap();
    let guarded = rule.guarded_semantics.as_mut().unwrap();
    let state_if = guarded
        .preorder(guarded.root().unwrap())
        .find(|node| *guarded.get_kind(*node) == SymKind::StateIf)
        .unwrap();
    let condition = guarded.children(state_if).next().unwrap();
    assert_eq!(*guarded.get_kind(condition), SymKind::UGt);
    let threshold = guarded.children(condition).nth(1).unwrap();
    assert!(matches!(
        guarded.get_leaf_data(threshold),
        Some(SymPayload::Int(value))
            if value.to_u64() == tir_adt::RoundingMode::TiesToAway as u64
    ));
    guarded.set_leaf_data(
        threshold,
        SymPayload::Int(tir_adt::APInt::new(
            3,
            tir_adt::RoundingMode::TowardPositive as u64,
        )),
    );

    assert!(prove_guarded_relaxations(std::slice::from_ref(rule)).is_err());
}

#[test]
fn fixed_float_rule_rejects_conditional_wrong_flag_update() {
    let context = Context::with_default_dialects();
    let mut rules = tir_riscv::get_isel_rules(
        &context,
        &[Feature::RV64I, Feature::F, Feature::F64, Feature::D],
    );
    let rule_index = rules
        .iter()
        .position(|rule| rule.name == "fadddrne")
        .unwrap();
    prove_guarded_relaxations(std::slice::from_ref(&rules[rule_index])).unwrap();
    let rule = &mut rules[rule_index];
    let guarded = rule.guarded_semantics.as_mut().unwrap();
    let root = guarded.root().unwrap();
    let flags_assign = guarded
        .preorder(root)
        .find(|node| {
            *guarded.get_kind(*node) == SymKind::StateAssign
                && guarded
                    .children(*node)
                    .nth(2)
                    .and_then(|field| guarded.get_leaf_data(field))
                    .is_some_and(|payload| {
                        matches!(payload, SymPayload::Int(value)
                            if value.to_u64() == tir::ResourceField::FpFlags.semantic_code())
                    })
        })
        .unwrap();
    let assign_children: Vec<_> = guarded.children(flags_assign).collect();
    let original_flags = *assign_children.last().unwrap();
    let condition = guarded.add_node(SymKind::Constant);
    guarded.set_leaf_data(condition, SymPayload::Int(tir_adt::APInt::new(1, 1)));
    let wrong_flags = guarded.add_node(SymKind::Constant);
    guarded.set_leaf_data(wrong_flags, SymPayload::Int(tir_adt::APInt::new(5, 0x1f)));
    let corrupted_flags = guarded.add_node(SymKind::If);
    for child in [condition, wrong_flags, original_flags] {
        guarded.add_edge(corrupted_flags, child);
    }
    let replacement = guarded.add_node(SymKind::StateAssign);
    for &child in &assign_children[..assign_children.len() - 1] {
        guarded.add_edge(replacement, child);
    }
    guarded.add_edge(replacement, corrupted_flags);
    let root_children: Vec<_> = guarded.children(root).collect();
    assert!(root_children.contains(&flags_assign));
    let replacement_root = guarded.add_node(SymKind::StateResult);
    for child in root_children {
        guarded.add_edge(
            replacement_root,
            if child == flags_assign {
                replacement
            } else {
                child
            },
        );
    }

    assert!(prove_guarded_relaxations(std::slice::from_ref(rule)).is_err());
}

fn features(march: &str, mattr: Option<&str>) -> Vec<Feature> {
    TargetConfig::parse(march, None, mattr)
        .expect("march should parse")
        .features()
        .to_vec()
}

#[test]
fn march_selects_extension_features() {
    assert_eq!(features("rv64i", None), vec![Feature::RV64I]);
    // On rv64 the M *W conjunctions (Zmmul64/RVM64) follow M automatically.
    assert_eq!(
        features("rv64im", None),
        vec![
            Feature::RV64I,
            Feature::RVM,
            Feature::Zmmul,
            Feature::Zmmul64,
            Feature::RVM64
        ]
    );
    assert_eq!(
        features("rv32imac", None),
        vec![
            Feature::RV32I,
            Feature::RVM,
            Feature::Zmmul,
            Feature::A,
            Feature::C,
            Feature::C32
        ]
    );
    assert_eq!(
        features("rv32i_zmmul", None),
        vec![Feature::RV32I, Feature::Zmmul]
    );
    // F/D select the float extensions; D implies F.
    assert_eq!(
        features("rv32if", None),
        vec![Feature::RV32I, Feature::F, Feature::Zicsr]
    );
    // On rv64 the internal D64 conjunction follows D automatically.
    assert_eq!(
        features("rv64ifd", None),
        vec![
            Feature::RV64I,
            Feature::F,
            Feature::D,
            Feature::Zicsr,
            Feature::F64,
            Feature::D64
        ]
    );
    assert_eq!(
        features("rv64id", None),
        vec![
            Feature::RV64I,
            Feature::F,
            Feature::D,
            Feature::Zicsr,
            Feature::F64,
            Feature::D64
        ]
    );
    assert_eq!(
        features("rv32ifd", None),
        vec![Feature::RV32I, Feature::F, Feature::D, Feature::Zicsr]
    );
    // G abbreviates IMAFD_Zicsr_Zifencei; M, A, F, D and Zifencei are modeled.
    let g = features("rv64gc_zba_zbb", None);
    assert!(g.contains(&Feature::RVM));
    assert!(g.contains(&Feature::A));
    assert!(g.contains(&Feature::F));
    assert!(g.contains(&Feature::D));
    assert!(g.contains(&Feature::Zifencei));
    // Bare architecture names select the generic, everything-on profile.
    assert_eq!(
        features("riscv64", None),
        vec![
            Feature::RV64I,
            Feature::Zmmul,
            Feature::RVM,
            Feature::Zmmul64,
            Feature::RVM64,
            Feature::F,
            Feature::D,
            Feature::D64,
            Feature::F64,
            Feature::C,
            Feature::C64,
            Feature::Zcd,
            Feature::A,
            Feature::A64,
            Feature::Zifencei,
            Feature::Zicsr,
            Feature::RVV,
            Feature::VF
        ]
    );
    assert!(!features("riscv32", None).contains(&Feature::RV64I));
}

#[test]
fn mattr_toggles_features() {
    assert_eq!(
        features("rv64i", Some("+m")),
        vec![
            Feature::RV64I,
            Feature::RVM,
            Feature::Zmmul,
            Feature::Zmmul64,
            Feature::RVM64
        ]
    );
    assert_eq!(
        features("rv64im", Some("-m,+zmmul")),
        vec![Feature::RV64I, Feature::Zmmul, Feature::Zmmul64]
    );
    assert!(TargetConfig::parse("rv64i", None, Some("+vector")).is_err());
    assert!(TargetConfig::parse("rv64i", None, Some("m")).is_err());
    assert!(TargetConfig::parse("rv64i", None, Some("-rv64i")).is_err());
}

#[test]
fn mcpu_resolves_machine_models() {
    let target = tir::backend::select_target("rv64im", Some("generic-ooo"), None).unwrap();
    assert_eq!(target.default_machine(), Some("rv64-ooo"));
    let target = tir::backend::select_target("rv32i", Some("scr1-3stage"), None).unwrap();
    assert_eq!(target.default_machine(), Some("scr1-3stage"));
    // The SCR1 model is declared `for [RV32I]`; rv64 must reject it.
    assert!(TargetConfig::parse("rv64i", Some("scr1-3stage"), None).is_err());
}

#[test]
fn isa_params_resolve_from_the_selected_base() {
    assert_eq!(
        tir_riscv::isa_params(&[Feature::RV32I]),
        vec![("ENCODING_UNIT", 32), ("XLEN", 32)]
    );
    assert_eq!(
        tir_riscv::isa_params(&[Feature::RV64I, Feature::RVM]),
        vec![("ENCODING_UNIT", 32), ("XLEN", 64)]
    );
    // VR is dynamically sized (width = vlenb, an architectural runtime value),
    // so it carries no static width here; its size is supplied by the machine.
    assert_eq!(
        tir_riscv::register_widths(&[Feature::RV32I]),
        vec![
            ("PC", 32),
            ("GPR", 32),
            ("FPR32", 32),
            ("FPR64", 64),
            ("FFLAGS", 5),
            ("FRM", 3),
            ("GPRC", 32),
            ("FPR64C", 64),
            ("FPR32C", 32),
            ("CSR", 32),
            ("VCSR", 32),
            ("VCFG", 32)
        ]
    );
    assert_eq!(
        tir_riscv::register_widths(&[Feature::RV64I]),
        vec![
            ("PC", 64),
            ("GPR", 64),
            ("FPR32", 32),
            ("FPR64", 64),
            ("FFLAGS", 5),
            ("FRM", 3),
            ("GPRC", 64),
            ("FPR64C", 64),
            ("FPR32C", 32),
            ("CSR", 64),
            ("VCSR", 64),
            ("VCFG", 64)
        ]
    );
    // Extensions alone resolve nothing; the base supplies XLEN.
    assert_eq!(tir_riscv::isa_params(&[Feature::RVM]), vec![]);
}

#[test]
fn counter_registers_follow_the_feature_set() {
    use tir::backend::PerfCounter;

    assert!(target("rv64i").counter_registers().is_empty());
    // RV64 reads the full 64-bit counters; RV32 adds the high-half CSRs.
    assert_eq!(target("rv64i_zicsr").counter_registers().len(), 3);
    let rv32 = target("rv32i_zicsr").counter_registers();
    assert_eq!(rv32.len(), 6);
    assert!(rv32.contains(&("CSR", 0xC80, PerfCounter::CyclesHigh)));
    assert!(rv32.contains(&("CSR", 0xC82, PerfCounter::InstructionsRetiredHigh)));
}
