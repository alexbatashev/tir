use tir::{
    Context, IRBuilder, IRFormatter, Operation, PassManager, TypeId,
    builtin::{FuncOp, IntegerType, ops},
    graph::{Dag, MutDag, OperandConstraint},
    sem_expr::{ExprKind, ExprPayload, ExprPostGraph},
};

use super::{
    EmitPlan, InstructionSelectPass, IselCostModel, Rule, RuleMatch, SelectionPressure, SemEGraph,
    SemNode, TargetIselModel, extension_rewrite, template_node,
};

fn symbol(g: &mut ExprPostGraph, id: u32) -> tir::graph::NodeId {
    let node = g.add_node(ExprKind::Symbol);
    g.set_leaf_data(node, ExprPayload::SymbolId(id));
    node
}

fn binary(
    g: &mut ExprPostGraph,
    kind: ExprKind,
    lhs: tir::graph::NodeId,
    rhs: tir::graph::NodeId,
) -> tir::graph::NodeId {
    let node = g.add_node(kind);
    g.add_edge(node, lhs);
    g.add_edge(node, rhs);
    node
}

fn atomic_pattern(kind: ExprKind) -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let lhs = symbol(&mut g, 0);
    let rhs = symbol(&mut g, 1);
    binary(&mut g, kind, lhs, rhs);
    g
}

fn add_mul_pattern() -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let x = symbol(&mut g, 0);
    let y = symbol(&mut g, 1);
    let mul = binary(&mut g, ExprKind::Mul, x, y);
    let z = symbol(&mut g, 2);
    binary(&mut g, ExprKind::Add, mul, z);
    g
}

fn emit_add(
    context: &Context,
    op: &tir::OperationRef,
    m: &RuleMatch,
) -> Result<EmitPlan, tir::PassError> {
    let lhs = m
        .value_binding(0)
        .unwrap_or_else(|| op.op().operands.first().copied().unwrap());
    let rhs = m
        .value_binding(2)
        .or_else(|| m.value_binding(1))
        .unwrap_or_else(|| op.op().operands[1]);
    let result_ty = context.get_value(op.op().results[0]).ty();
    Ok(EmitPlan::single(Box::new(
        ops::addi(context, lhs, rhs, result_ty).build(),
    )))
}

fn emit_mul(
    context: &Context,
    op: &tir::OperationRef,
    _m: &RuleMatch,
) -> Result<EmitPlan, tir::PassError> {
    let result_ty = context.get_value(op.op().results[0]).ty();
    Ok(EmitPlan::single(Box::new(
        ops::muli(context, op.op().operands[0], op.op().operands[1], result_ty).build(),
    )))
}

#[test]
fn pbqp_selector_consumes_internal_nodes_of_selected_pattern() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let x = context.create_value(i32_ty, None);
    let y = context.create_value(i32_ty, None);
    let z = context.create_value(i32_ty, None);
    let x_id = x.id();
    let y_id = y.id();
    let z_id = z.id();
    let region = context.create_region();
    let block = context.create_block(vec![x, y, z]);
    region.add_block(block.id());

    let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
    let mul_result = mul.result();
    fb.insert(mul);
    let add = ops::addi(&context, mul_result, z_id, i32_ty).build();
    let add_result = add.result();
    fb.insert(add);
    fb.insert(ops::r#return(&context, add_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    let rules = vec![
        Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
        Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    pm.run(&context, context.get_op(module.id()))
        .expect("pass pipeline should succeed");

    let body_ops: Vec<_> = context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect();
    assert_eq!(body_ops, vec!["addi", "return"]);

    let mut buf = String::new();
    let mut fmt = IRFormatter::new(&mut buf);
    module.print(&mut fmt).expect("print lowered module");
    assert!(!buf.contains("muli"));
}

#[test]
fn rule_validation_rejects_missing_atomic_materializer() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let x = context.create_value(i32_ty, None);
    let y = context.create_value(i32_ty, None);
    let z = context.create_value(i32_ty, None);
    let x_id = x.id();
    let y_id = y.id();
    let z_id = z.id();
    let region = context.create_region();
    let block = context.create_block(vec![x, y, z]);
    region.add_block(block.id());

    // A standalone Mul that no rule can root and no parent match can consume:
    // the e-graph cover is infeasible, so selection fails naming the kind.
    let _ = z_id;
    let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
    let mul_result = mul.result();
    fb.insert(mul);
    fb.insert(ops::r#return(&context, mul_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    let rules = vec![Rule::new(
        "add",
        atomic_pattern(ExprKind::Add),
        10,
        emit_add,
    )];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    let err = pm
        .run(&context, context.get_op(module.id()))
        .expect_err("incomplete rule set should be rejected");
    assert!(err.to_string().contains("Mul"));
}

#[test]
fn pbqp_selector_does_not_consume_shared_internal_nodes() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let x = context.create_value(i32_ty, None);
    let y = context.create_value(i32_ty, None);
    let z = context.create_value(i32_ty, None);
    let x_id = x.id();
    let y_id = y.id();
    let z_id = z.id();
    let region = context.create_region();
    let block = context.create_block(vec![x, y, z]);
    region.add_block(block.id());

    let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
    let mul_result = mul.result();
    fb.insert(mul);
    let add0 = ops::addi(&context, mul_result, z_id, i32_ty).build();
    let add0_result = add0.result();
    fb.insert(add0);
    let add1 = ops::addi(&context, mul_result, add0_result, i32_ty).build();
    let add1_result = add1.result();
    fb.insert(add1);
    fb.insert(ops::r#return(&context, add1_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    let rules = vec![
        Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
        Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    pm.run(&context, context.get_op(module.id()))
        .expect("pass pipeline should succeed");

    let body_ops: Vec<_> = context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect();
    assert_eq!(body_ops, vec!["muli", "addi", "addi", "return"]);
}

fn add_mul_add_pattern() -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let a = symbol(&mut g, 0);
    let b = symbol(&mut g, 1);
    let inner = binary(&mut g, ExprKind::Add, a, b);
    let c = symbol(&mut g, 2);
    let mul = binary(&mut g, ExprKind::Mul, inner, c);
    let d = symbol(&mut g, 3);
    binary(&mut g, ExprKind::Add, mul, d);
    g
}

/// A cost model that makes the fused `add-mul` rule prohibitively expensive,
/// so selection must fall back to the atomic `mul` + `add` cover.
struct NoFusionCostModel;

impl IselCostModel for NoFusionCostModel {
    fn node_cost(
        &self,
        _context: &Context,
        _op: &tir::OperationRef,
        rule: &Rule,
        _m: &RuleMatch,
        _pressure: &SelectionPressure,
        _target: &dyn TargetIselModel,
    ) -> u64 {
        if rule.name == "add-mul" {
            1000
        } else {
            rule.base_cost as u64
        }
    }
}

struct NoFusionTarget {
    cost: NoFusionCostModel,
}

impl TargetIselModel for NoFusionTarget {
    fn cost_model(&self) -> &dyn IselCostModel {
        &self.cost
    }
}

#[test]
fn cost_model_override_changes_selection() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let x = context.create_value(i32_ty, None);
    let y = context.create_value(i32_ty, None);
    let z = context.create_value(i32_ty, None);
    let (x_id, y_id, z_id) = (x.id(), y.id(), z.id());
    let region = context.create_region();
    let block = context.create_block(vec![x, y, z]);
    region.add_block(block.id());

    let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let mul = ops::muli(&context, x_id, y_id, i32_ty).build();
    let mul_result = mul.result();
    fb.insert(mul);
    let add = ops::addi(&context, mul_result, z_id, i32_ty).build();
    let add_result = add.result();
    fb.insert(add);
    fb.insert(ops::r#return(&context, add_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    let rules = vec![
        Rule::new("add-mul", add_mul_pattern(), 1, emit_add),
        Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(
            InstructionSelectPass::new(rules).with_target_model(Box::new(NoFusionTarget {
                cost: NoFusionCostModel,
            })),
        );
    pm.run(&context, context.get_op(module.id()))
        .expect("pass pipeline should succeed");

    let body_ops: Vec<_> = context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect();
    // With fusion priced out, the default add-mul cost-1 win is overridden.
    assert_eq!(body_ops, vec!["muli", "addi", "return"]);
}

#[test]
fn composite_rule_falls_back_to_atomic_cover() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let a = context.create_value(i32_ty, None);
    let b = context.create_value(i32_ty, None);
    let c = context.create_value(i32_ty, None);
    let d = context.create_value(i32_ty, None);
    let (a_id, b_id, c_id, d_id) = (a.id(), b.id(), c.id(), d.id());
    let region = context.create_region();
    let block = context.create_block(vec![a, b, c, d]);
    region.add_block(block.id());

    let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let add0 = ops::addi(&context, a_id, b_id, i32_ty).build();
    let add0_result = add0.result();
    fb.insert(add0);
    let mul = ops::muli(&context, add0_result, c_id, i32_ty).build();
    let mul_result = mul.result();
    fb.insert(mul);
    let add1 = ops::addi(&context, mul_result, d_id, i32_ty).build();
    let add1_result = add1.result();
    fb.insert(add1);
    fb.insert(ops::r#return(&context, add1_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    // `add-mul-add` requires a `Mul(Add(_,_),_)` subpattern that no rule
    // provides; the pass synthesizes it. Selection must remain valid and, with
    // fusion priced high, fall back to the atomic cover.
    let rules = vec![
        Rule::new("add-mul-add", add_mul_add_pattern(), 100, emit_add),
        Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
        Rule::new("mul", atomic_pattern(ExprKind::Mul), 10, emit_mul),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    pm.run(&context, context.get_op(module.id()))
        .expect("pass pipeline should succeed");

    let body_ops: Vec<_> = context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect();
    assert_eq!(body_ops, vec!["addi", "muli", "addi", "return"]);
}

/// A binary pattern constrained to a specific result type via the pattern
/// graph's actual-type annotation (the channel a typed rule would use).
fn typed_binary_pattern(kind: ExprKind, ty: TypeId) -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let lhs = symbol(&mut g, 0);
    let rhs = symbol(&mut g, 1);
    let root = binary(&mut g, kind, lhs, rhs);
    g.set_actual_type(root, ty);
    g
}

fn emit_sub(
    context: &Context,
    op: &tir::OperationRef,
    _m: &RuleMatch,
) -> Result<EmitPlan, tir::PassError> {
    let result_ty = context.get_value(op.op().results[0]).ty();
    Ok(EmitPlan::single(Box::new(
        ops::subi(context, op.op().operands[0], op.op().operands[1], result_ty).build(),
    )))
}

#[test]
fn selection_is_type_aware() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let i64_ty = IntegerType::new(&context, 64);
    let a32v = context.create_value(i32_ty, None);
    let b32v = context.create_value(i32_ty, None);
    let a64v = context.create_value(i64_ty, None);
    let b64v = context.create_value(i64_ty, None);
    let (a32, b32, a64, b64) = (a32v.id(), b32v.id(), a64v.id(), b64v.id());
    let region = context.create_region();
    let block = context.create_block(vec![a32v, b32v, a64v, b64v]);
    region.add_block(block.id());

    let func = ops::func(&context, "demo", i64_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let add32 = ops::addi(&context, a32, b32, i32_ty).build();
    fb.insert(add32);
    let add64 = ops::addi(&context, a64, b64, i64_ty).build();
    let add64_result = add64.result();
    fb.insert(add64);
    fb.insert(ops::r#return(&context, add64_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    // Same opcode (Add), two result widths. The i32-constrained rule must only
    // fire on the i32 add; the i64 add falls back to the width-agnostic rule.
    let rules = vec![
        Rule::new(
            "add.i32",
            typed_binary_pattern(ExprKind::Add, i32_ty),
            1,
            emit_sub,
        ),
        Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    pm.run(&context, context.get_op(module.id()))
        .expect("pass pipeline should succeed");

    let body_ops: Vec<_> = context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect();
    // i32 add -> the type-constrained rule (subi stand-in); i64 add -> fallback addi.
    assert_eq!(body_ops, vec!["subi", "addi", "return"]);
}

/// Build `add(add(a,b), c)` over i32 values and select it with a fused
/// `Add(Add(_,_),_)` rule whose *internal* node carries `inner_width` as a type
/// constraint (plus an untyped atomic `add` fallback). Returns the lowered op
/// names. Fusion (the `subi` marker) only happens when the inner constraint
/// agrees with the inferred i32 type of the inner add.
fn run_inner_typed_fusion(inner_width: Option<u32>) -> Vec<&'static str> {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i32_ty = IntegerType::new(&context, 32);
    let a = context.create_value(i32_ty, None);
    let b = context.create_value(i32_ty, None);
    let c = context.create_value(i32_ty, None);
    let (a_id, b_id, c_id) = (a.id(), b.id(), c.id());
    let region = context.create_region();
    let block = context.create_block(vec![a, b, c]);
    region.add_block(block.id());

    let func = ops::func(&context, "demo", i32_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let add0 = ops::addi(&context, a_id, b_id, i32_ty).build();
    let add0_result = add0.result();
    fb.insert(add0);
    let add1 = ops::addi(&context, add0_result, c_id, i32_ty).build();
    let add1_result = add1.result();
    fb.insert(add1);
    fb.insert(ops::r#return(&context, add1_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    // Fused pattern Add(Add(s0, s1), s2); optionally constrain the inner Add.
    let mut pattern = ExprPostGraph::new();
    let s0 = symbol(&mut pattern, 0);
    let s1 = symbol(&mut pattern, 1);
    let inner = binary(&mut pattern, ExprKind::Add, s0, s1);
    let s2 = symbol(&mut pattern, 2);
    binary(&mut pattern, ExprKind::Add, inner, s2);
    if let Some(width) = inner_width {
        pattern.set_actual_type(inner, IntegerType::new(&context, width));
    }

    let rules = vec![
        Rule::new("add-add", pattern, 1, emit_sub),
        Rule::new("add", atomic_pattern(ExprKind::Add), 10, emit_add),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    pm.run(&context, context.get_op(module.id()))
        .expect("pass pipeline should succeed");

    context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect()
}

#[test]
fn internal_node_type_constraint_is_enforced() {
    // Inner add inferred as i32 from i32 operands. A matching i32 constraint
    // (or no constraint) lets the fused rule consume it; an i64 constraint
    // forbids the match, falling back to two atomic adds.
    assert_eq!(run_inner_typed_fusion(Some(32)), vec!["subi", "return"]);
    assert_eq!(run_inner_typed_fusion(None), vec!["subi", "return"]);
    assert_eq!(
        run_inner_typed_fusion(Some(64)),
        vec!["addi", "addi", "return"]
    );
}

/// The square problem: a sub-word sign extension has no single RISC-V base
/// instruction. Equality saturation with the discovered `SExt -> slli/srai`
/// bridge must make the `SExt(v@i16, 64)` class selectable as an arithmetic
/// shift by `W - n = 48`, exactly the `srai` of the `add, slli, srai` idiom.
#[test]
fn saturation_bridges_sign_extension_to_shift_pair() {
    use tir::egraph::SaturationLimits;
    use tir::graph::{OperandConstraint, Pattern, PatternExpr};
    use tir::sem_expr::{ExprKind, ExprPayload};
    use tir::utils::APInt;

    let ctx = Context::with_default_dialects();
    let i16 = IntegerType::new(&ctx, 16);
    let i64 = IntegerType::new(&ctx, 64);

    // SExt(v @ i16, 64), typed i64 — the program graph node no RV64 base
    // instruction can root.
    let mut egraph = SemEGraph::new();
    let v = egraph.add(
        template_node(ExprKind::Symbol, Some(ExprPayload::SymbolId(0)), Some(i16)),
        &[],
        None,
    );
    let width = egraph.add(
        template_node(
            ExprKind::Constant,
            Some(ExprPayload::Int(APInt::new(64, 64))),
            None,
        ),
        &[],
        None,
    );
    let sext = egraph.add(
        template_node(ExprKind::SExt, None, Some(i64)),
        &[v, width],
        None,
    );

    let rewrite = extension_rewrite(ExprKind::SExt, ExprKind::ShiftRightArithmetic);
    egraph.saturate(
        &ctx,
        std::slice::from_ref(&rewrite),
        SaturationLimits::default(),
    );

    // The sext class now also contains the shift-pair realization.
    assert!(
        egraph
            .nodes(sext)
            .iter()
            .any(|&id| egraph.get_node(id).kind == ExprKind::ShiftRightArithmetic),
        "saturation should add the arithmetic-shift bridge to the sext class"
    );

    // An immediate `srai` pattern matches the class, with shift amount 64-16=48.
    let mut srai = Pattern::<SemNode, ()>::new(());
    let rs1 = srai.add_node(PatternExpr::Boundary);
    srai.set_duplicable(rs1, true);
    let imm = srai.add_node(PatternExpr::Boundary);
    srai.set_duplicable(imm, true);
    srai.set_operand_constraint(imm, OperandConstraint::Immediate);
    let root = srai.add_node(PatternExpr::Node(template_node(
        ExprKind::ShiftRightArithmetic,
        None,
        None,
    )));
    srai.add_edge(root, rs1);
    srai.add_edge(root, imm);
    srai.set_root(root);

    let matches = egraph.ematch(&ctx, &srai);
    let m = matches
        .iter()
        .find(|m| egraph.find(m.root()) == egraph.find(sext))
        .expect("an immediate srai must match the sext class after saturation");
    let shift_amount = egraph
        .nodes(m.binding(imm))
        .iter()
        .find_map(|&id| match egraph.get_node(id).payload.as_ref() {
            Some(ExprPayload::Int(v)) => Some(v.to_u64()),
            _ => None,
        })
        .expect("the srai shift amount must be a constant");
    assert_eq!(shift_amount, 48);
}

fn shift_imm_pattern(kind: ExprKind) -> ExprPostGraph {
    let mut g = ExprPostGraph::new();
    let rs1 = symbol(&mut g, 0);
    let imm = symbol(&mut g, 1);
    binary(&mut g, kind, rs1, imm);
    g
}

fn emit_shift_marker(
    marker: ExprKind,
) -> impl Fn(&Context, &tir::OperationRef, &RuleMatch) -> Result<EmitPlan, tir::PassError> {
    move |context, op, m| {
        let rs1 = m
            .value_binding(0)
            .ok_or(tir::PassError::RewriteFailed(op.op().id))?;
        let result_ty = context.get_value(op.op().results[0]).ty();
        // The shift amount is an immediate (m.int_binding(1)); operands beyond the
        // mnemonic don't matter for this test, so the source register is reused.
        let built: Box<dyn Operation> = match marker {
            ExprKind::ShiftLeft => Box::new(ops::shli(context, rs1, rs1, result_ty).build()),
            _ => Box::new(ops::shrsi(context, rs1, rs1, result_ty).build()),
        };
        Ok(EmitPlan::single(built))
    }
}

fn emit_slli(
    context: &Context,
    op: &tir::OperationRef,
    m: &RuleMatch,
) -> Result<EmitPlan, tir::PassError> {
    emit_shift_marker(ExprKind::ShiftLeft)(context, op, m)
}

fn emit_srai(
    context: &Context,
    op: &tir::OperationRef,
    m: &RuleMatch,
) -> Result<EmitPlan, tir::PassError> {
    emit_shift_marker(ExprKind::ShiftRightArithmetic)(context, op, m)
}

/// End-to-end square: `extsi(addi(a, b) : i16) : i64` lowers to `add, slli, srai`.
/// The `add` covers the addi; saturation bridges the un-selectable sign extension
/// into a `slli`/`srai` pair, and multi-instruction emission materializes the
/// introduced `slli` (an e-class with no original op) before the `srai`.
#[test]
fn square_sign_extension_lowers_to_shift_pair() {
    let context = Context::with_default_dialects();
    let module = ops::module(&context, None).build();

    let i16_ty = IntegerType::new(&context, 16);
    let i64_ty = IntegerType::new(&context, 64);
    let a = context.create_value(i16_ty, None);
    let b = context.create_value(i16_ty, None);
    let (a_id, b_id) = (a.id(), b.id());
    let region = context.create_region();
    let block = context.create_block(vec![a, b]);
    region.add_block(block.id());

    let func = ops::func(&context, "square", i64_ty, Some(region.id())).build();
    let mut fb = IRBuilder::new(func.body());
    let add = ops::addi(&context, a_id, b_id, i16_ty).build();
    let add_result = add.result();
    fb.insert(add);
    let ext = ops::extsi(&context, add_result, i64_ty).build();
    let ext_result = ext.result();
    fb.insert(ext);
    fb.insert(ops::r#return(&context, ext_result).build());

    let mut mb = IRBuilder::new(module.body());
    mb.insert(func);
    mb.insert(ops::module_end(&context).build());

    let rules = vec![
        Rule::new("add", atomic_pattern(ExprKind::Add), 1, emit_add),
        Rule::new("slli", shift_imm_pattern(ExprKind::ShiftLeft), 1, emit_slli)
            .with_operand_constraints(vec![(1, OperandConstraint::Immediate)]),
        Rule::new(
            "srai",
            shift_imm_pattern(ExprKind::ShiftRightArithmetic),
            1,
            emit_srai,
        )
        .with_operand_constraints(vec![(1, OperandConstraint::Immediate)]),
    ];

    let mut pm = PassManager::new();
    pm.nest(FuncOp::name())
        .add_pass(InstructionSelectPass::new(rules));
    pm.run(&context, context.get_op(module.id()))
        .expect("square should select");

    let body_ops: Vec<_> = context
        .get_region(region.id())
        .iter(context.clone())
        .next()
        .unwrap()
        .op_ids()
        .into_iter()
        .map(|op_id| context.get_op(op_id).name)
        .collect();
    // add (from the addi), then the slli/srai sign-extension idiom, then return.
    assert_eq!(body_ops, vec!["addi", "shli", "shrsi", "return"]);
}
