//! InstCombine: a cost-driven peephole over the builtin arith dialect, built on
//! the generic e-graph. For each function it lifts the pure integer dataflow into
//! an e-graph (values crossing block boundaries — block args, loads, control ops —
//! enter as opaque leaves, so structured and unstructured control flow are handled
//! uniformly), saturates with the static [`rules`], extracts the cheapest form per
//! value under [`InstCost`], and rebuilds only the ops that improved.
//!
//! Untouched e-nodes carry the [`crate::graph::Dag`] `original_op`/`actual_type`
//! annotations through saturation, so an unchanged value is reattached to its
//! original op (or a dominating equivalent) instead of being rematerialized.

mod rules;

use std::collections::{HashMap, HashSet};

use crate::analysis::DominatorTree;
use crate::egraph::{EClassId, EGraph, SaturationLimits};
use crate::graph::{Dag, MutDag, NodeId};
use crate::sem_expr::{ExprKind, ExprPayload, ExprPostGraph};
use crate::{
    BlockId, Commutative, Context, OpId, Operation, OperationRef, Pass, PassError, PassTarget,
    Rewriter, TypeId, ValueId,
    builtin::{self, ConstantOp, FuncOp, InstCost},
    utils::APInt,
};

type ArithEGraph = EGraph<ExprKind, ExprPayload>;

#[derive(Default)]
pub struct InstCombinePass;

impl InstCombinePass {
    pub fn new() -> Self {
        Self
    }
}

crate::register_pass!(InstCombinePass, "instcombine");

impl Pass for InstCombinePass {
    fn name(&self) -> &'static str {
        "instcombine"
    }

    fn target(&self) -> PassTarget {
        PassTarget::Operation(FuncOp::name())
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(());
        }

        let dom = DominatorTree::new(context, op.op().id);
        let layout = Layout::collect(context, &dom);

        let seed = Seeder::build(context, &layout);
        let mut eg = EGraph::new();
        let mut class_of_value: HashMap<ValueId, EClassId> = HashMap::new();
        for (&value, &node) in &seed.node_of_value {
            if seed.op_results.contains(&value) {
                class_of_value.insert(value, eg.add_dag(&seed.graph, node));
            }
        }

        eg.saturate(context, &rules::arith_rules(), SaturationLimits::default());
        let best = eg.extract_best(cost);

        let reconstruct = Reconstruct {
            eg: &eg,
            best: &best,
            layout: &layout,
            dom: &dom,
            context,
        };
        reconstruct.apply(rewriter, &class_of_value)
    }
}

/// Per-e-node cost: leaves are free, multiply is dear via [`InstCost`], every
/// other modeled instruction is a single cheap op.
fn cost(kind: &ExprKind, _children: &[u64]) -> u64 {
    match kind {
        ExprKind::Symbol | ExprKind::Constant => 0,
        ExprKind::Mul => builtin::MulIOp::COST as u64,
        _ => 1,
    }
}

fn modeled_kind(kind: ExprKind) -> bool {
    use ExprKind::*;
    matches!(
        kind,
        Add | Sub | Mul | And | Or | Xor | ShiftLeft | ShiftRightLogic | ShiftRightArithmetic
    )
}

/// Lifts the function's pure integer dataflow into one annotated [`ExprPostGraph`],
/// sharing a node per value so congruent subexpressions hash-cons in the e-graph.
struct Seeder {
    graph: ExprPostGraph,
    node_of_value: HashMap<ValueId, NodeId>,
    /// Values defined by a modeled or constant op (the rewrite candidates).
    op_results: HashSet<ValueId>,
}

impl Seeder {
    fn build(context: &Context, layout: &Layout) -> Self {
        let mut seeder = Seeder {
            graph: ExprPostGraph::new(),
            node_of_value: HashMap::new(),
            op_results: HashSet::new(),
        };
        for op_id in layout.ops(context) {
            seeder.seed_op(context, op_id);
        }
        seeder
    }

    fn seed_op(&mut self, context: &Context, op_id: OpId) {
        let instance = context.get_op(op_id);

        if let Some(constant) = instance.clone().as_op::<ConstantOp>() {
            let result = constant.result();
            let node = self.graph.add_node(ExprKind::Constant);
            let width = type_width(context, context.get_value(result).ty()).unwrap_or(64);
            self.graph.set_leaf_data(
                node,
                ExprPayload::Int(APInt::new_signed(width, const_value(&instance))),
            );
            self.annotate(node, op_id, context.get_value(result).ty());
            self.node_of_value.insert(result, node);
            self.op_results.insert(result);
            return;
        }

        let mut tmp = ExprPostGraph::new();
        let kind = instance
            .clone()
            .as_dyn_op()
            .semantic_expr(&mut tmp)
            .map(|root| *tmp.get_node(root));

        match kind {
            Some(kind) if modeled_kind(kind) && instance.operands.len() == 2 => {
                let result = instance.results[0];
                let ty = context.get_value(result).ty();
                // Children must exist before the parent: a [`PostOrderDag`] requires
                // every edge to point to a strictly lower index.
                let mut children: Vec<NodeId> = instance
                    .operands
                    .iter()
                    .map(|&operand| self.value_node(operand))
                    .collect();
                if instance.as_interface::<dyn Commutative>().is_some() {
                    children.sort_by_key(|n| n.index());
                }
                let node = self.graph.add_node(kind);
                for child in children {
                    self.graph.add_edge(node, child);
                }
                self.annotate(node, op_id, ty);
                self.node_of_value.insert(result, node);
                self.op_results.insert(result);
            }
            // Anything we don't model contributes its results as opaque leaves.
            _ => {
                for &result in &instance.results {
                    self.value_node(result);
                }
            }
        }
    }

    /// The graph node standing for `value`, creating an opaque leaf the first time
    /// a non-modeled value (block arg, function param, unmodeled result) is seen.
    fn value_node(&mut self, value: ValueId) -> NodeId {
        if let Some(&node) = self.node_of_value.get(&value) {
            return node;
        }
        let node = self.graph.add_node(ExprKind::Symbol);
        self.graph.set_leaf_data(node, ExprPayload::Value(value));
        self.node_of_value.insert(value, node);
        node
    }

    fn annotate(&mut self, node: NodeId, op_id: OpId, ty: TypeId) {
        self.graph.set_original_op(node, op_id);
        self.graph.set_actual_type(node, ty);
    }
}

/// Rebuilds improved values out of the extracted e-graph.
struct Reconstruct<'a> {
    eg: &'a ArithEGraph,
    best: &'a HashMap<EClassId, (NodeId, u64)>,
    layout: &'a Layout,
    dom: &'a DominatorTree,
    context: &'a Context,
}

impl Reconstruct<'_> {
    fn apply(
        &self,
        rewriter: &mut Rewriter,
        class_of_value: &HashMap<ValueId, EClassId>,
    ) -> Result<(), PassError> {
        // A modeled operator op is rewritten when its class' cheapest e-node is not
        // the op itself. Constants are never rewritten — they are already minimal.
        let mut targets: Vec<(OpId, ValueId, EClassId)> = Vec::new();
        let mut rewritten: HashSet<OpId> = HashSet::new();
        for (&value, &class) in class_of_value {
            let Some(op_id) = self.context.get_value(value).defining_op() else {
                continue;
            };
            if self.context.get_op(op_id).as_op::<ConstantOp>().is_some() {
                continue;
            }
            let class = self.eg.find(class);
            let best = self.best[&class].0;
            if self.eg.get_original_op(best) != Some(op_id) {
                targets.push((op_id, value, class));
                rewritten.insert(op_id);
            }
        }

        for (op_id, value, class) in targets {
            let target = self.layout.op_ref(self.context, op_id);
            let ty = self.context.get_value(value).ty();
            let mut memo = HashMap::new();
            let new_value = self.emit(class, ty, &target, &rewritten, rewriter, &mut memo)?;
            self.context.replace_value_uses(value, new_value);
            rewriter.erase_op(&target)?;
        }
        Ok(())
    }

    /// Materialize the cheapest form of `class`, inserting new ops before `target`.
    /// Bottoms out at opaque leaves and at unchanged dominating ops, which it reuses
    /// directly; rule-introduced nodes and rewritten ops are built fresh.
    fn emit(
        &self,
        class: EClassId,
        expected_ty: TypeId,
        target: &OperationRef,
        rewritten: &HashSet<OpId>,
        rewriter: &mut Rewriter,
        memo: &mut HashMap<EClassId, ValueId>,
    ) -> Result<ValueId, PassError> {
        let class = self.eg.find(class);
        if let Some(&value) = memo.get(&class) {
            return Ok(value);
        }
        let node = self.best[&class].0;
        let ty = self.eg.get_actual_type(node).unwrap_or(expected_ty);

        let value = match self.eg.get_node(node) {
            ExprKind::Symbol => match self.eg.get_leaf_data(node) {
                Some(ExprPayload::Value(v)) => *v,
                other => panic!("opaque leaf without a value: {other:?}"),
            },
            ExprKind::Constant => {
                let v = match self.eg.get_leaf_data(node) {
                    Some(ExprPayload::Int(v)) => v.clone(),
                    other => panic!("constant without a value: {other:?}"),
                };
                let new_op = builtin::ops::constant(self.context, v.to_i64(), ty).build();
                rewriter.insert_op_before(target, &new_op)?;
                new_op.result()
            }
            &kind => {
                if let Some(origin) = self.eg.get_original_op(node)
                    && !rewritten.contains(&origin)
                    && self.layout.dominates(self.dom, origin, target.op().id)
                {
                    self.context.get_op(origin).results[0]
                } else {
                    let children = self.eg.child_classes(node);
                    let mut operands = Vec::with_capacity(children.len());
                    for child in children {
                        operands.push(self.emit(child, ty, target, rewritten, rewriter, memo)?);
                    }
                    self.build_binop(kind, &operands, ty, target, rewriter)?
                }
            }
        };
        memo.insert(class, value);
        Ok(value)
    }

    /// Build the concrete builtin op for `kind`, insert it before `target`, and
    /// return its result. Each generated op exposes an inherent `result()`, so we
    /// dispatch per kind rather than through a trait object.
    fn build_binop(
        &self,
        kind: ExprKind,
        operands: &[ValueId],
        ty: TypeId,
        target: &OperationRef,
        rewriter: &mut Rewriter,
    ) -> Result<ValueId, PassError> {
        use crate::builtin::ops;
        let (a, b) = (operands[0], operands[1]);
        macro_rules! emit {
            ($builder:ident) => {{
                let new_op = ops::$builder(self.context, a, b, ty).build();
                rewriter.insert_op_before(target, &new_op)?;
                new_op.result()
            }};
        }
        Ok(match kind {
            ExprKind::Add => emit!(addi),
            ExprKind::Sub => emit!(subi),
            ExprKind::Mul => emit!(muli),
            ExprKind::And => emit!(andi),
            ExprKind::Or => emit!(ori),
            ExprKind::Xor => emit!(xori),
            ExprKind::ShiftLeft => emit!(shli),
            ExprKind::ShiftRightLogic => emit!(shrui),
            ExprKind::ShiftRightArithmetic => emit!(shrsi),
            other => panic!("not a modeled binary operator: {other:?}"),
        })
    }
}

/// Where every operation lives, lifting block-level dominance to operations.
struct Layout {
    position: HashMap<OpId, (BlockId, usize)>,
    blocks: Vec<BlockId>,
}

impl Layout {
    fn collect(context: &Context, dom: &DominatorTree) -> Self {
        let mut blocks: Vec<BlockId> = (0..dom.len())
            .map(NodeId::from_index)
            .filter_map(|node| dom.block(node))
            .collect();
        blocks.sort_by_key(BlockId::number);

        let mut position = HashMap::new();
        for &block_id in &blocks {
            for (index, op_id) in context.get_block(block_id).op_ids().into_iter().enumerate() {
                position.insert(op_id, (block_id, index));
            }
        }
        Self { position, blocks }
    }

    fn ops(&self, context: &Context) -> Vec<OpId> {
        self.blocks
            .iter()
            .flat_map(|&block_id| context.get_block(block_id).op_ids())
            .collect()
    }

    fn dominates(&self, dom: &DominatorTree, a: OpId, b: OpId) -> bool {
        let (Some(&(a_block, a_index)), Some(&(b_block, b_index))) =
            (self.position.get(&a), self.position.get(&b))
        else {
            return false;
        };
        if a_block == b_block {
            a_index <= b_index
        } else {
            dom.dominates(a_block, b_block)
        }
    }

    fn op_ref(&self, context: &Context, op_id: OpId) -> OperationRef {
        let block = self
            .position
            .get(&op_id)
            .map(|(b, _)| context.get_block(*b));
        OperationRef::new(context.get_op(op_id), block, None)
    }
}

fn type_width(context: &Context, ty: TypeId) -> Option<u32> {
    let data = context.get_type_data(ty);
    (data.as_ref() as &dyn std::any::Any)
        .downcast_ref::<builtin::IntegerType>()
        .map(builtin::IntegerType::width)
}

fn const_value(instance: &crate::OpInstance) -> i64 {
    instance
        .attributes
        .iter()
        .find(|attr| attr.name == "value")
        .and_then(|attr| match attr.value {
            crate::attributes::AttributeValue::Int(v) => Some(v),
            _ => None,
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, IRFormatter, OpId, Operation, PassManager,
        builtin::{IntegerType, ops as b},
    };

    use super::InstCombinePass;

    fn run(context: &Context, func: OpId) {
        let mut pm = PassManager::new();
        pm.add_pass(InstCombinePass::new());
        pm.run(context, context.get_op(func)).expect("instcombine");
    }

    fn print_func(func: &impl Operation) -> String {
        let mut out = String::new();
        let mut fmt = IRFormatter::new(&mut out);
        func.print(&mut fmt).expect("print");
        out
    }

    /// A single-block function `f(x) { body; return last }`.
    fn func_with<F>(
        context: &Context,
        params: usize,
        build: F,
    ) -> (crate::builtin::FuncOp, Vec<crate::ValueId>)
    where
        F: FnOnce(&Context, &mut IRBuilder, &[crate::ValueId]) -> crate::ValueId,
    {
        let i32_ty = IntegerType::new(context, 32);
        let args: Vec<_> = (0..params)
            .map(|_| context.create_value(i32_ty, None))
            .collect();
        let region = context.create_region();
        let block = context.create_block(args.clone());
        region.add_block(block.id());
        let func = b::func(context, "f", i32_ty, Some(region.id())).build();
        let arg_ids: Vec<_> = args.iter().map(|v| v.id()).collect();
        let mut builder = IRBuilder::new(func.body());
        let result = build(context, &mut builder, &arg_ids);
        builder.insert(b::r#return(context, result).build());
        (func, arg_ids)
    }

    #[test]
    fn multiply_by_power_of_two_becomes_shift() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let (func, _) = func_with(&context, 1, |ctx, builder, args| {
            let four = builder.insert(b::constant(ctx, 4, i32_ty).build());
            builder
                .insert(b::muli(ctx, args[0], four.result(), i32_ty).build())
                .result()
        });

        run(&context, func.id());

        let out = print_func(&func);
        assert!(out.contains("shli"), "expected a shift:\n{out}");
        assert!(!out.contains("muli"), "multiply should be gone:\n{out}");
    }

    #[test]
    fn add_zero_is_identity() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let (func, args) = func_with(&context, 1, |ctx, builder, args| {
            let zero = builder.insert(b::constant(ctx, 0, i32_ty).build());
            builder
                .insert(b::addi(ctx, args[0], zero.result(), i32_ty).build())
                .result()
        });

        run(&context, func.id());

        let out = print_func(&func);
        assert!(!out.contains("addi"), "add should be gone:\n{out}");
        assert!(
            out.contains(&format!("return %{}", args[0].number())),
            "return should read the parameter directly:\n{out}"
        );
    }

    #[test]
    fn folds_constant_expression() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let (func, _) = func_with(&context, 0, |ctx, builder, _| {
            let two = builder.insert(b::constant(ctx, 2, i32_ty).build());
            let three = builder.insert(b::constant(ctx, 3, i32_ty).build());
            builder
                .insert(b::addi(ctx, two.result(), three.result(), i32_ty).build())
                .result()
        });

        run(&context, func.id());

        let out = print_func(&func);
        assert!(!out.contains("addi"), "add should be folded:\n{out}");
        assert!(
            out.contains("value = 5"),
            "expected the folded constant 5:\n{out}"
        );
    }

    #[test]
    fn subtract_self_is_zero() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let (func, _) = func_with(&context, 1, |ctx, builder, args| {
            builder
                .insert(b::subi(ctx, args[0], args[0], i32_ty).build())
                .result()
        });

        run(&context, func.id());

        let out = print_func(&func);
        assert!(!out.contains("subi"), "subtract should be gone:\n{out}");
        assert!(
            out.contains("value = 0"),
            "expected a zero constant:\n{out}"
        );
    }

    /// A value combined in one block feeds an op in a successor block. The
    /// cross-block value enters the successor's e-graph as an opaque leaf, so the
    /// successor's `addi _, 0` still folds to it without modeling the branch.
    #[test]
    fn combines_across_unstructured_branch() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let param = context.create_value(i32_ty, None);
        let param_id = param.id();

        let region = context.create_region();
        let entry = context.create_block(vec![param]);
        let next = context.create_block(vec![]);
        region.add_block(entry.id());
        region.add_block(next.id());
        let func = b::func(&context, "fwd", i32_ty, Some(region.id())).build();

        let mut entry_b = IRBuilder::new(entry.clone());
        let two = entry_b.insert(b::constant(&context, 2, i32_ty).build());
        let scaled = entry_b
            .insert(b::muli(&context, param_id, two.result(), i32_ty).build())
            .result();
        entry_b.insert(b::br(&context, vec![], next.id()).build());

        let mut next_b = IRBuilder::new(next.clone());
        let zero = next_b.insert(b::constant(&context, 0, i32_ty).build());
        let added = next_b.insert(b::addi(&context, scaled, zero.result(), i32_ty).build());
        let added_id = added.id();
        let ret_id = next_b
            .insert(b::r#return(&context, added.result()).build())
            .id();

        run(&context, func.id());

        // The `addi _, 0` folded away; the multiply became a shift.
        assert!(
            !context.has_operation(added_id),
            "addi should be folded away"
        );
        let ret_operand = context.get_op(ret_id).operands[0];
        let def = context
            .get_value(ret_operand)
            .defining_op()
            .expect("defined");
        assert_eq!(context.get_op(def).name, "shli");
    }
}
