//! InstCombine: an equality-saturation peephole over the builtin dialect. It seeds
//! the function's pure integer dataflow into an e-graph of real IR values, saturates
//! with a dialect-supplied [`Rule`] set plus generic constant folding (every op's
//! [`crate::ConstantFold`] interface, derived from its `sem`), then extracts the
//! cheapest equivalent form per value by [`crate::OpCost`] and rewrites the values
//! that improved.
//!
//! The engine holds no op-specific knowledge: identity, commutativity, cost,
//! folding and constant-reading all come from op interfaces; op construction is
//! owned by the rewrites (via their `emit`) and one dialect-supplied constant
//! builder. See [`rules`] for the builtin ruleset.

mod rules;
mod term;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::analysis::DominatorTree;
use crate::egraph::{EClassId, EGraph, SaturationLimits};
use crate::graph::{Dag, GenericDag, MutDag, NodeId};
use crate::{
    BlockId, Context, OpId, Operation, OperationRef, Pass, PassError, PassTarget, RegionGuard,
    RegionId, Rewriter, TypeId, ValueId,
    builtin::{self, FuncOp},
    utils::APInt,
};

use rules::{Ruleset, builtin_ruleset};
use term::{Leaf, Term};

/// Builds a constant op of `ty` holding a value — the one piece of op construction
/// the engine needs, supplied by the dialect so the engine stays op-agnostic.
type MakeConstant = dyn Fn(&Context, &APInt, TypeId) -> Box<dyn Operation> + Send + Sync;

type ArithEGraph = EGraph<Term, Leaf>;

pub struct InstCombinePass {
    ruleset: Ruleset,
    make_constant: Arc<MakeConstant>,
}

impl InstCombinePass {
    pub fn new() -> Self {
        Self {
            ruleset: builtin_ruleset(),
            make_constant: Arc::new(|context, value: &APInt, ty| {
                Box::new(builtin::ops::constant(context, value.to_i64(), ty).build())
                    as Box<dyn Operation>
            }),
        }
    }
}

impl Default for InstCombinePass {
    fn default() -> Self {
        Self::new()
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
        let mut eg = ArithEGraph::new();
        let mut class_of_value: HashMap<ValueId, EClassId> = HashMap::new();
        for &value in &seed.op_results {
            let node = seed.node_of_value[&value];
            class_of_value.insert(value, eg.add_dag(&seed.graph, node));
        }

        // The class of every seeded value (op results plus opaque leaves), so a
        // guard assumption can union the branch condition with a boolean constant.
        let mut value_class: HashMap<ValueId, EClassId> = HashMap::new();
        for &value in seed.node_of_value.keys() {
            let class = class_of_value
                .get(&value)
                .copied()
                .unwrap_or_else(|| eg.add(Term::Opaque, &[], Some(Leaf::Value(value))));
            value_class.insert(value, class);
        }

        let mut driver = Driver {
            context,
            eg,
            class_of_value,
            value_class,
            ruleset: &self.ruleset,
            make_constant: self.make_constant.as_ref(),
            layout: &layout,
            constants: &seed.constants,
        };
        let body = context.get_op(op.op().id).regions[0];
        driver.process_region(body, rewriter)
    }
}

/// Walks the region tree, rewriting each region under the assumptions that hold
/// there. Entering a guarded region (via [`RegionGuard`]) pushes a context and
/// unions the branch condition with a boolean constant; leaving it pops the context,
/// so the assumption never leaks out. Nested regions without a guard are still
/// visited, just under the enclosing context.
struct Driver<'a> {
    context: &'a Context,
    eg: ArithEGraph,
    class_of_value: HashMap<ValueId, EClassId>,
    value_class: HashMap<ValueId, EClassId>,
    ruleset: &'a Ruleset,
    make_constant: &'a MakeConstant,
    layout: &'a Layout,
    constants: &'a HashSet<OpId>,
}

impl Driver<'_> {
    fn process_region(
        &mut self,
        region: RegionId,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        self.eg.saturate(
            self.context,
            &self.ruleset.rewrites,
            SaturationLimits::default(),
        );
        let best = self.eg.extract_best(|term, _| match term {
            Term::Op { cost, .. } => *cost as u64,
            _ => 0,
        });

        let op_ids: Vec<OpId> = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .flat_map(|block| self.context.get_block(block.id()).op_ids())
            .collect();

        // Rewrite each non-constant value op in this region whose cheapest form is no
        // longer itself.
        {
            let materializer = Materializer {
                context: self.context,
                eg: &self.eg,
                ruleset: self.ruleset,
                make_constant: self.make_constant,
                best: &best,
            };
            for &op_id in &op_ids {
                if self.constants.contains(&op_id) {
                    continue;
                }
                let instance = self.context.get_op(op_id);
                if instance.results.len() != 1 {
                    continue;
                }
                let value = instance.results[0];
                let Some(&class) = self.class_of_value.get(&value) else {
                    continue;
                };
                let ty = self.context.get_value(value).ty();
                let target = self.layout.op_ref(self.context, op_id);
                let mut memo = HashMap::new();
                let new_value = materializer.emit(class, ty, &target, rewriter, &mut memo)?;
                if new_value != value {
                    self.context.replace_value_uses(value, new_value);
                    rewriter.erase_op(&target)?;
                }
            }
        }

        // Recurse into nested regions, assuming each guard's fact inside its region.
        // Skip ops the write-back above may have erased.
        for &op_id in &op_ids {
            if !self.context.has_operation(op_id) {
                continue;
            }
            let instance = self.context.get_op(op_id);
            if instance.regions.is_empty() {
                continue;
            }
            let assumptions: HashMap<RegionId, (ValueId, bool)> = instance
                .clone()
                .as_interface::<dyn RegionGuard>()
                .map(|guard| {
                    guard
                        .guarded_regions()
                        .into_iter()
                        .map(|(region, value, holds)| (region, (value, holds)))
                        .collect()
                })
                .unwrap_or_default();
            for &sub in &instance.regions {
                match assumptions.get(&sub) {
                    Some(&(value, holds)) => {
                        self.eg.push_context();
                        self.inject(value, holds);
                        self.process_region(sub, rewriter)?;
                        self.eg.pop_context();
                    }
                    None => self.process_region(sub, rewriter)?,
                }
            }
        }
        Ok(())
    }

    /// Assume `value == holds` inside the current context by unioning its class with
    /// the matching boolean constant.
    fn inject(&mut self, value: ValueId, holds: bool) {
        let cond = self
            .value_class
            .get(&value)
            .copied()
            .unwrap_or_else(|| self.eg.add(Term::Opaque, &[], Some(Leaf::Value(value))));
        let constant = self.eg.add(
            Term::Const,
            &[],
            Some(Leaf::Int(APInt::new(1, holds as u64))),
        );
        self.eg.union(cond, constant);
        self.eg.rebuild();
    }
}

/// Lifts the function's pure integer dataflow into one [`GenericDag`] over [`Term`]
/// labels, sharing a node per value so congruent subexpressions hash-cons. Reads ops
/// generically through their interfaces — no op is named.
struct Seeder {
    graph: GenericDag<Term, Leaf>,
    node_of_value: HashMap<ValueId, NodeId>,
    /// Op results to consider rewriting, in program order.
    op_results: Vec<ValueId>,
    /// Constant ops, excluded from rewriting.
    constants: HashSet<OpId>,
}

impl Seeder {
    fn build(context: &Context, layout: &Layout) -> Self {
        let mut seeder = Seeder {
            graph: GenericDag::new(),
            node_of_value: HashMap::new(),
            op_results: Vec::new(),
            constants: HashSet::new(),
        };
        for op_id in layout.ops(context) {
            seeder.seed_op(context, op_id);
        }
        seeder
    }

    fn seed_op(&mut self, context: &Context, op_id: OpId) {
        let instance = context.get_op(op_id);

        if let Some(constant) = instance.clone().as_interface::<dyn crate::ConstantLike>() {
            let result = instance.results[0];
            let node = self.graph.add_node(Term::Const);
            self.graph
                .set_leaf_data(node, Leaf::Int(constant.constant_value()));
            self.annotate(node, op_id, context.get_value(result).ty());
            self.node_of_value.insert(result, node);
            self.op_results.push(result);
            self.constants.insert(op_id);
            return;
        }

        if is_pure_value(&instance) {
            let result = instance.results[0];
            let ty = context.get_value(result).ty();
            let mut children: Vec<NodeId> = instance
                .operands
                .iter()
                .map(|&operand| self.value_node(operand))
                .collect();
            let term = Term::of_op(&instance);
            if let Term::Op {
                commutative: true, ..
            } = term
            {
                children.sort_by_key(|n| n.index());
            }
            let node = self.graph.add_node(term);
            for child in children {
                self.graph.add_edge(node, child);
            }
            self.annotate(node, op_id, ty);
            self.node_of_value.insert(result, node);
            self.op_results.push(result);
            return;
        }

        for &result in &instance.results {
            self.value_node(result);
        }
    }

    /// The node standing for `value`, creating an opaque leaf the first time an
    /// external value (block arg, unmodeled result) is seen.
    fn value_node(&mut self, value: ValueId) -> NodeId {
        if let Some(&node) = self.node_of_value.get(&value) {
            return node;
        }
        let node = self.graph.add_node(Term::Opaque);
        self.graph.set_leaf_data(node, Leaf::Value(value));
        self.node_of_value.insert(value, node);
        node
    }

    fn annotate(&mut self, node: NodeId, op_id: OpId, ty: TypeId) {
        self.graph.set_original_op(node, op_id);
        self.graph.set_actual_type(node, ty);
    }
}

/// A pure value op the e-graph can reason about: one result, no regions, and a
/// declared semantic expression (so it computes a value with no side effects).
fn is_pure_value(instance: &Arc<crate::OpInstance>) -> bool {
    instance.results.len() == 1
        && instance.regions.is_empty()
        && instance
            .clone()
            .as_dyn_op()
            .semantic_expr(&mut crate::sem_expr::ExprPostGraph::new())
            .is_some()
}

/// Rebuilds improved values out of the extracted e-graph. A chosen node is either an
/// existing IR value (reused) or one a rewrite introduced (built by that rewrite's
/// `emit`, found through the node's saturation provenance) — never by op identity.
struct Materializer<'a> {
    context: &'a Context,
    eg: &'a ArithEGraph,
    ruleset: &'a Ruleset,
    make_constant: &'a MakeConstant,
    best: &'a HashMap<EClassId, (NodeId, u64)>,
}

impl Materializer<'_> {
    fn emit(
        &self,
        class: EClassId,
        expected_ty: TypeId,
        target: &OperationRef,
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
            Term::Opaque => match self.eg.get_leaf_data(node) {
                Some(Leaf::Value(v)) => *v,
                other => panic!("opaque leaf without a value: {other:?}"),
            },
            // A seeded constant reuses its op; a folded/introduced one is built.
            Term::Const => {
                if let Some(origin) = self.eg.get_original_op(node) {
                    self.context.get_op(origin).results[0]
                } else {
                    self.build_constant(node, ty, target, rewriter)?
                }
            }
            // A seeded op reuses its existing value (operand updates propagate via
            // `replace_value_uses`); a rule-introduced one is built by its rewrite.
            Term::Op { .. } => {
                if let Some(origin) = self.eg.get_original_op(node) {
                    self.context.get_op(origin).results[0]
                } else {
                    self.build_introduced(node, ty, target, rewriter, memo)?
                }
            }
        };
        memo.insert(class, value);
        Ok(value)
    }

    fn build_constant(
        &self,
        node: NodeId,
        ty: TypeId,
        target: &OperationRef,
        rewriter: &mut Rewriter,
    ) -> Result<ValueId, PassError> {
        let value = match self.eg.get_leaf_data(node) {
            Some(Leaf::Int(v)) => v.clone(),
            other => panic!("constant without a value: {other:?}"),
        };
        let op = (self.make_constant)(self.context, &value, ty);
        let id = op.id();
        rewriter.insert_op_before(target, op.as_ref())?;
        Ok(self.context.get_op(id).results[0])
    }

    fn build_introduced(
        &self,
        node: NodeId,
        ty: TypeId,
        target: &OperationRef,
        rewriter: &mut Rewriter,
        memo: &mut HashMap<EClassId, ValueId>,
    ) -> Result<ValueId, PassError> {
        let producer = self
            .eg
            .producer(node)
            .expect("a rule-introduced op records its producing rewrite");
        let emit = self.ruleset.emits[producer]
            .as_ref()
            .expect("the producing rewrite supplies an emit for the op it introduces");
        let mut operands = Vec::new();
        for child in self.eg.child_classes(node) {
            operands.push(self.emit(child, ty, target, rewriter, memo)?);
        }
        emit(self.context, &operands, ty, target, rewriter)
    }
}

/// Where every operation lives: used to iterate ops in program order and to build an
/// [`OperationRef`] insertion target.
struct Layout {
    block_of: HashMap<OpId, BlockId>,
    blocks: Vec<BlockId>,
}

impl Layout {
    fn collect(context: &Context, dom: &DominatorTree) -> Self {
        let mut blocks: Vec<BlockId> = (0..dom.len())
            .map(NodeId::from_index)
            .filter_map(|node| dom.block(node))
            .collect();
        blocks.sort_by_key(BlockId::number);

        let mut block_of = HashMap::new();
        for &block_id in &blocks {
            for op_id in context.get_block(block_id).op_ids() {
                block_of.insert(op_id, block_id);
            }
        }
        Self { block_of, blocks }
    }

    fn ops(&self, context: &Context) -> Vec<OpId> {
        self.blocks
            .iter()
            .flat_map(|&block_id| context.get_block(block_id).op_ids())
            .collect()
    }

    fn op_ref(&self, context: &Context, op_id: OpId) -> OperationRef {
        let block = self.block_of.get(&op_id).map(|b| context.get_block(*b));
        OperationRef::new(context.get_op(op_id), block, None)
    }
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

    /// Real saturation: folding `1 * 4 -> 4` only then enables strength-reducing
    /// `x * 4 -> x << 2`. The second rule matches a form the first produced.
    #[test]
    fn saturation_chains_fold_then_strength_reduce() {
        let context = Context::with_default_dialects();
        let i32_ty = IntegerType::new(&context, 32);
        let (func, _) = func_with(&context, 1, |ctx, builder, args| {
            let one = builder.insert(b::constant(ctx, 1, i32_ty).build());
            let four = builder.insert(b::constant(ctx, 4, i32_ty).build());
            let folded = builder.insert(b::muli(ctx, one.result(), four.result(), i32_ty).build());
            builder
                .insert(b::muli(ctx, args[0], folded.result(), i32_ty).build())
                .result()
        });

        run(&context, func.id());

        let out = print_func(&func);
        assert!(
            out.contains("shli"),
            "expected a shift after folding:\n{out}"
        );
        assert!(
            !out.contains("muli"),
            "both multiplies should be gone:\n{out}"
        );
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

    /// Flow sensitivity: inside `if (cond)` the engine assumes `cond == 1`, so
    /// `muli cond, y` folds to `y` there — while the identical multiply in the entry
    /// block, where `cond` is unknown, is left alone.
    #[test]
    fn assumes_condition_inside_guarded_region() {
        use crate::{Operand, scf};

        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let cond = context.create_value(i1, None);
        let y = context.create_value(i1, None);
        let (cond_id, y_id) = (cond.id(), y.id());

        let region = context.create_region();
        let entry = context.create_block(vec![cond, y]);
        region.add_block(entry.id());

        let then_region = context.create_region();
        let then_block = context.create_block(vec![]);
        then_region.add_block(then_block.id());
        let mut tb = IRBuilder::new(then_block);
        tb.insert(b::muli(&context, cond_id, y_id, i1).build());
        tb.insert(scf::ops::r#yield(&context, Operand::none()).build());

        let else_region = context.create_region();
        let else_block = context.create_block(vec![]);
        else_region.add_block(else_block.id());
        IRBuilder::new(else_block).insert(scf::ops::r#yield(&context, Operand::none()).build());

        let func = b::func(&context, "f", i1, Some(region.id())).build();
        let mut eb = IRBuilder::new(entry);
        eb.insert(b::muli(&context, cond_id, y_id, i1).build());
        eb.insert(
            scf::ops::r#if(
                &context,
                cond_id,
                Some(then_region.id()),
                Some(else_region.id()),
            )
            .build(),
        );
        eb.insert(b::r#return(&context, y_id).build());

        run(&context, func.id());

        let out = print_func(&func);
        assert_eq!(
            out.matches("muli").count(),
            1,
            "the guarded multiply should fold under cond==true; the entry one stays:\n{out}"
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
