use tir::backend::isel::{
    prove_guarded_relaxations, EmitRequest, RegisterCapability, RegisterRequirement, Rule,
    RuleMatch, LATENCY_COST_SCALE,
};
use tir::sem::{SemGraph, StateAccessKind, StateFieldKind, StateResourceKind, SymKind};
use tir::{Context, Operation, PassError};

use super::fixtures::{binary, constant, nary, symbol};

fn no_emit(_: &Context, _: &EmitRequest, _: &RuleMatch) -> Result<Box<dyn Operation>, PassError> {
    unreachable!()
}

fn rule(kind: SymKind, rounded: SymKind, width: u32, replacement: u64, mode: u64) -> Rule {
    let arity = kind.arity();
    let mut candidate = SemGraph::new();
    let inputs: Vec<_> = (0..arity)
        .map(|i| symbol(&mut candidate, i as u32))
        .collect();
    nary(&mut candidate, kind, &inputs);

    let mut full = SemGraph::new();
    let mut inputs: Vec<_> = (0..arity).map(|i| symbol(&mut full, i as u32)).collect();
    inputs.push(constant(&mut full, mode, 3));
    let outcome = nary(&mut full, rounded, &inputs);
    let result = nary(&mut full, SymKind::FPValue, &[outcome]);
    let ordered = binary(&mut full, SymKind::Ge, result, result);
    let bits = constant(&mut full, replacement, width);
    let nan = nary(&mut full, SymKind::AsFloat, &[bits]);
    nary(&mut full, SymKind::If, &[ordered, result, nan]);
    let register = RegisterRequirement::whole(RegisterCapability::float(width));
    Rule {
        guarded_semantics: Some(full),
        operand_registers: (0..arity).map(|i| (i as u32, register)).collect(),
        result_register: Some(register),
        ..Rule::new("ieee-result", candidate, LATENCY_COST_SCALE, no_emit)
    }
}

#[test]
fn ieee_arithmetic_accepts_quiet_nan_payload_choice() {
    for (width, nan) in [(32, 0xffc01234), (64, 0x7ff8000000001234)] {
        for (kind, rounded) in [
            (SymKind::FAdd, SymKind::FAddRound),
            (SymKind::FSub, SymKind::FSubRound),
            (SymKind::FMul, SymKind::FMulRound),
            (SymKind::FDiv, SymKind::FDivRound),
            (SymKind::Sqrt, SymKind::SqrtRound),
            (SymKind::Fma, SymKind::FmaRound),
        ] {
            assert!(prove_guarded_relaxations(&[rule(kind, rounded, width, nan, 0)]).is_ok());
        }
    }
}

#[test]
fn ieee_arithmetic_rejects_finite_result_corruption() {
    use tir::graph::Dag;

    let mut rule = rule(SymKind::FAdd, SymKind::FAddRound, 32, 0x7fc00000, 0);
    let full = rule.guarded_semantics.as_mut().unwrap();
    let original = full.root().unwrap();
    let result = full.children(original).nth(1).unwrap();
    let ordered = binary(full, SymKind::Ge, result, result);
    let zero = constant(full, 0, 32);
    let zero_float = nary(full, SymKind::AsFloat, &[zero]);
    nary(full, SymKind::If, &[ordered, zero_float, original]);
    assert!(prove_guarded_relaxations(&[rule]).is_err());
}

#[test]
fn ieee_arithmetic_rejects_signaling_nan_replacement() {
    for (width, nan) in [(32, 0x7f800001), (64, 0x7ff0000000000001)] {
        let rule = rule(SymKind::FAdd, SymKind::FAddRound, width, nan, 0);
        assert!(prove_guarded_relaxations(&[rule]).is_err());
    }
}

#[test]
fn ieee_arithmetic_rejects_different_operands() {
    let mut rule = rule(SymKind::FSub, SymKind::FSubRound, 32, 0x7fc00000, 0);
    let mut candidate = SemGraph::new();
    let a = symbol(&mut candidate, 0);
    let b = symbol(&mut candidate, 1);
    binary(&mut candidate, SymKind::FSub, b, a);
    rule.pattern = candidate;
    assert!(prove_guarded_relaxations(&[rule]).is_err());
}

#[test]
fn ieee_arithmetic_rejects_different_operation() {
    let rule = rule(SymKind::FSub, SymKind::FAddRound, 32, 0x7fc00000, 0);
    assert!(prove_guarded_relaxations(&[rule]).is_err());
}

#[test]
fn ieee_arithmetic_rejects_different_rounding() {
    for mode in 1..=4 {
        let rule = rule(SymKind::FAdd, SymKind::FAddRound, 32, 0x7fc00000, mode);
        assert!(prove_guarded_relaxations(&[rule]).is_err());
    }
}

#[test]
fn float_copy_keeps_nan_payload_bits() {
    let mut candidate = SemGraph::new();
    symbol(&mut candidate, 0);
    let mut full = SemGraph::new();
    let value = symbol(&mut full, 0);
    let ordered = binary(&mut full, SymKind::Ge, value, value);
    let nan = constant(&mut full, 0x7fc00000, 32);
    let nan_float = nary(&mut full, SymKind::AsFloat, &[nan]);
    nary(&mut full, SymKind::If, &[ordered, value, nan_float]);
    let register = RegisterRequirement::whole(RegisterCapability::float(32));
    let rule = Rule {
        guarded_semantics: Some(full),
        operand_registers: vec![(0, register)],
        result_register: Some(register),
        ..Rule::new("float-copy", candidate, LATENCY_COST_SCALE, no_emit)
    };
    assert!(prove_guarded_relaxations(&[rule]).is_err());
}

#[test]
fn ieee_arithmetic_preserves_signed_zero() {
    use tir::graph::Dag;

    let mut rule = rule(SymKind::FAdd, SymKind::FAddRound, 32, 0x7fc00000, 0);
    let full = rule.guarded_semantics.as_mut().unwrap();
    let original = full.root().unwrap();
    let result = full.children(original).nth(1).unwrap();
    let zero = constant(full, 0, 32);
    let zero_float = nary(full, SymKind::AsFloat, &[zero]);
    let is_zero = binary(full, SymKind::Eq, result, zero_float);
    nary(full, SymKind::If, &[is_zero, zero_float, original]);
    assert!(prove_guarded_relaxations(&[rule]).is_err());
}

#[test]
fn value_only_rule_rejects_guarded_fp_flags_change() {
    for preceding_unchanged_state in [false, true] {
        let mut candidate = SemGraph::new();
        symbol(&mut candidate, 0);

        let mut guarded = SemGraph::new();
        let value = symbol(&mut guarded, 0);
        let initial = symbol(&mut guarded, 1);
        let resource = constant(&mut guarded, StateResourceKind::FpEnvironment as u64, 2);
        let field = constant(&mut guarded, StateFieldKind::FpFlags as u64, 2);
        let access = constant(&mut guarded, StateAccessKind::Change as u64, 1);
        let flags = constant(&mut guarded, 1, 5);
        let changed = nary(
            &mut guarded,
            SymKind::StateAssign,
            &[initial, resource, field, access, flags],
        );
        let mut results = vec![value];
        if preceding_unchanged_state {
            results.push(symbol(&mut guarded, 2));
        }
        results.push(changed);
        nary(&mut guarded, SymKind::StateResult, &results);

        let rule = Rule {
            guarded_semantics: Some(guarded),
            ..Rule::new("changed-flags", candidate, LATENCY_COST_SCALE, no_emit)
        };
        assert!(prove_guarded_relaxations(&[rule]).is_err());
    }
}

fn restored_flags_rule(corrupt_restore: bool) -> Rule {
    let mut candidate = SemGraph::new();
    symbol(&mut candidate, 0);

    let mut guarded = SemGraph::new();
    let value = symbol(&mut guarded, 0);
    let initial = symbol(&mut guarded, 1);
    let resource = constant(&mut guarded, StateResourceKind::FpEnvironment as u64, 2);
    let field = constant(&mut guarded, StateFieldKind::FpFlags as u64, 2);
    let read = constant(&mut guarded, StateAccessKind::Read as u64, 1);
    let change = constant(&mut guarded, StateAccessKind::Change as u64, 1);
    let saved = nary(
        &mut guarded,
        SymKind::StateRead,
        &[initial, resource, field],
    );
    let read_state = nary(
        &mut guarded,
        SymKind::StateAssign,
        &[initial, resource, field, read, saved],
    );
    let raised = constant(&mut guarded, 0x1f, 5);
    let changed = nary(
        &mut guarded,
        SymKind::StateAssign,
        &[read_state, resource, field, change, raised],
    );
    let width = constant(&mut guarded, 64, 32);
    let saved_xlen = binary(&mut guarded, SymKind::ZExt, saved, width);
    let high = constant(&mut guarded, 4, 32);
    let low = constant(&mut guarded, 0, 32);
    let mut restored_flags = nary(&mut guarded, SymKind::Extract, &[saved_xlen, high, low]);
    if corrupt_restore {
        let bit = constant(&mut guarded, 1, 5);
        restored_flags = binary(&mut guarded, SymKind::Xor, restored_flags, bit);
    }
    let restored = nary(
        &mut guarded,
        SymKind::StateAssign,
        &[changed, resource, field, change, restored_flags],
    );
    nary(&mut guarded, SymKind::StateResult, &[value, restored]);

    Rule {
        guarded_semantics: Some(guarded),
        ..Rule::new("restored-flags", candidate, LATENCY_COST_SCALE, no_emit)
    }
}

#[test]
fn value_only_rule_accepts_restored_fp_flags() {
    assert!(prove_guarded_relaxations(&[restored_flags_rule(false)]).is_ok());
}

#[test]
fn value_only_rule_rejects_wrong_restored_fp_flags() {
    assert!(prove_guarded_relaxations(&[restored_flags_rule(true)]).is_err());
}

#[test]
fn value_only_rule_rejects_trap_before_restored_fp_flags() {
    use tir::graph::Dag;

    let mut rule = restored_flags_rule(false);
    let guarded = rule.guarded_semantics.as_mut().unwrap();
    let root = guarded.root().unwrap();
    let root_children: Vec<_> = guarded.children(root).collect();
    let restored_children: Vec<_> = guarded.children(root_children[1]).collect();
    let raised = constant(guarded, 1, 5);
    let enabled = constant(guarded, 1, 5);
    let trapped = nary(
        guarded,
        SymKind::StateTrap,
        &[restored_children[0], raised, enabled],
    );
    let restored = nary(
        guarded,
        SymKind::StateAssign,
        &[
            trapped,
            restored_children[1],
            restored_children[2],
            restored_children[3],
            restored_children[4],
        ],
    );
    nary(guarded, SymKind::StateResult, &[root_children[0], restored]);

    assert!(prove_guarded_relaxations(&[rule]).is_err());
}

#[test]
fn value_only_rule_rejects_nonterminating_update_before_restore() {
    use tir::graph::Dag;

    let mut rule = restored_flags_rule(false);
    let guarded = rule.guarded_semantics.as_mut().unwrap();
    let root = guarded.root().unwrap();
    let root_children: Vec<_> = guarded.children(root).collect();
    let restored_children: Vec<_> = guarded.children(root_children[1]).collect();
    let changed_children: Vec<_> = guarded.children(restored_children[0]).collect();
    let zero = constant(guarded, 0, 5);
    let always = constant(guarded, 1, 1);
    let loop_value = nary(guarded, SymKind::Loop, &[zero, zero, zero, always]);
    let changed = nary(
        guarded,
        SymKind::StateAssign,
        &[
            changed_children[0],
            changed_children[1],
            changed_children[2],
            changed_children[3],
            loop_value,
        ],
    );
    let restored = nary(
        guarded,
        SymKind::StateAssign,
        &[
            changed,
            restored_children[1],
            restored_children[2],
            restored_children[3],
            restored_children[4],
        ],
    );
    nary(guarded, SymKind::StateResult, &[root_children[0], restored]);

    assert!(prove_guarded_relaxations(&[rule]).is_err());
}
