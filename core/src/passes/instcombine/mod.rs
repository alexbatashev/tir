//! InstCombine: an equality-saturation peephole. It seeds the function's regions
//! ([`seed`], which reads gates off the ops' own interfaces) into a
//! [`tir_symbolic`] e-graph of real IR values, saturates, extracts the cheapest
//! form per value by [`crate::OpCost`], and rewrites what
//! improved. Flow-sensitive facts ride the e-graph's scoped assumptions: a
//! structured region pushes its guard's condition around its body, and unstructured
//! `cond_br` facts ([`crate::analysis::DominatingEdgeFacts`]) are asserted in
//! dominator-tree DFS order — each block's own guard fact scoped once and inherited
//! by the blocks it dominates — then popped on the way back up.
//!
//! The engine holds no op-specific knowledge — identity, cost, folding and
//! constant-reading come from op interfaces; op construction is owned by the rewrites.

pub(crate) mod rules;
mod seed;
mod state;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use tir_symbolic::egraph::{EGraph, Extraction, Id};

use std::rc::Rc;

use crate::analysis::{DominatingEdgeFacts, DominatorTree};
use crate::graph::Dag;
use crate::{
    AnalysisManager, BlockId, Conditional, ConstantLike, Context, MemoryRead, OpId, OpInstance,
    OperationRef, Pass, PassError, PassTarget, RegionId, Rewriter, TokenScope, TypeId, ValueId,
    builtin::{FuncOp, ops},
    utils::APInt,
};

use crate::sem::node::cost;
use crate::sem::{Prov, SemNode as Node, SymKind};
use rules::{Ruleset, builtin_ruleset};

const ITER_LIMIT: usize = 30;
const NODE_LIMIT: usize = 100_000;

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
        PassTarget::operation::<FuncOp>()
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        analyses: &AnalysisManager,
    ) -> Result<(), PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(());
        }
        let root = op.op().id;
        let seeded = seed::seed(context, root);
        let ruleset = builtin_ruleset(context, &seeded);
        let mut driver = Driver {
            context,
            eg: seeded.eg,
            value_class: seeded.value_class,
            arg_block: RefCell::new(seeded.arg_block),
            dom: analyses.get::<DominatorTree>(context, root),
            edge_facts: analyses.get::<DominatingEdgeFacts>(context, root),
            ruleset,
            gamma_ports: RefCell::new(HashMap::new()),
            theta_ports: RefCell::new(HashMap::new()),
            port_bindings: RefCell::new(Vec::new()),
        };
        let body = context.get_op(root).regions[0];
        driver.process_region(body, rewriter)?;
        // Rewrites erase and insert ops within blocks but never touch the block
        // graph, so dominance survives; the value graph does not.
        Ok(())
    }
}

/// Rewrites each region under the assumptions that hold there, and *before* its
/// children's scopes open so the base classes a child scope reads are final.
struct Driver<'a> {
    context: &'a Context,
    eg: EGraph<Node>,
    value_class: HashMap<ValueId, Id>,
    /// The block each block argument belongs to. A commit growing a carried port
    /// adds arguments of its own, which the dominance check has to locate too.
    arg_block: RefCell<HashMap<ValueId, BlockId>>,
    dom: Rc<DominatorTree>,
    edge_facts: Rc<DominatingEdgeFacts>,
    ruleset: Ruleset,
    /// The value port already grown on a gate for a pair of arm classes, so every
    /// read the gate answers takes the one port.
    gamma_ports: RefCell<HashMap<(OpId, Id, Id), ValueId>>,
    /// The same for a loop, over the classes it carries from and to.
    theta_ports: RefCell<HashMap<(OpId, Id, Id), ThetaPort>>,
    /// The θ classes standing for a port argument while that port's latch is
    /// being built: inside the body the carry *is* the argument.
    port_bindings: RefCell<Vec<(Id, ValueId)>>,
}

/// A carried port a θ commit grew: the argument each of the loop's regions reads
/// the carried value as, and the result the loop leaves it in.
struct ThetaPort {
    arguments: Vec<(RegionId, ValueId)>,
    result: ValueId,
}

impl Driver<'_> {
    fn process_region(
        &mut self,
        region: RegionId,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        self.eg
            .saturate(&self.ruleset.rewrites, ITER_LIMIT, NODE_LIMIT);
        crate::memstats::egraph_census("instcombine", &self.eg);
        let extraction = self.eg.extract_best(|_, node| cost(node));

        let blocks: Vec<BlockId> = self
            .context
            .get_region(region)
            .iter(self.context.clone())
            .map(|block| block.id())
            .collect();
        let region_blocks: HashSet<BlockId> = blocks.iter().copied().collect();

        // Rewrite blocks in dominator-tree DFS order so each block's own guard
        // fact, pushed as a scope, is inherited by the blocks it dominates. The
        // region entry dominates the rest of the region, so it roots the DFS.
        let mut visited = HashSet::new();
        if let Some(&entry) = blocks.first() {
            self.rewrite_block_tree(entry, &region_blocks, &extraction, rewriter, &mut visited)?;
        }
        // Blocks unreachable in the dominator tree carry no inherited fact.
        for &block in &blocks {
            if visited.insert(block) {
                for op_id in self.context.get_block(block).op_ids() {
                    self.rewrite_op(op_id, &extraction, rewriter)?;
                }
            }
        }

        let op_ids: Vec<OpId> = blocks
            .iter()
            .flat_map(|&block| self.context.get_block(block).op_ids())
            .collect();
        self.recurse(&op_ids, rewriter)
    }

    /// Rewrite `block` then its dominator subtree (restricted to this region).
    /// A block's own guard fact holds throughout that subtree, so inject it once
    /// under a fresh scope and let the nesting carry it down — no re-assertion.
    fn rewrite_block_tree(
        &mut self,
        block: BlockId,
        region_blocks: &HashSet<BlockId>,
        parent_extraction: &Extraction<Node>,
        rewriter: &mut Rewriter,
        visited: &mut HashSet<BlockId>,
    ) -> Result<(), PassError> {
        if !visited.insert(block) {
            return Ok(());
        }
        // A condition without a seeded class cannot be assumed; skip, don't panic.
        let pushed = match self.edge_facts.own_fact(block) {
            Some(fact) if self.value_class.contains_key(&fact.condition) => {
                self.eg.push_context();
                self.inject(fact.condition, fact.holds);
                self.eg
                    .saturate(&self.ruleset.rewrites, ITER_LIMIT, NODE_LIMIT);
                true
            }
            _ => false,
        };
        // A pushed fact changes what extracts cheapest; reuse the parent's otherwise.
        let local = pushed.then(|| self.eg.extract_best(|_, node| cost(node)));
        let extraction = local.as_ref().unwrap_or(parent_extraction);

        for op_id in self.context.get_block(block).op_ids() {
            self.rewrite_op(op_id, extraction, rewriter)?;
        }

        if let Some(node) = self.dom.node_of(block) {
            let children: Vec<BlockId> = self
                .dom
                .children(node)
                .filter_map(|child| self.dom.block(child))
                .filter(|child| region_blocks.contains(child))
                .collect();
            for child in children {
                self.rewrite_block_tree(child, region_blocks, extraction, rewriter, visited)?;
            }
        }

        if pushed {
            self.eg.pop_context();
        }
        Ok(())
    }

    /// Replace `op_id`'s value with its cheapest equivalent form, if that improved.
    fn rewrite_op(
        &self,
        op_id: OpId,
        extraction: &Extraction<Node>,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        if !self.context.has_operation(op_id) {
            return Ok(());
        }
        let instance = self.context.get_op(op_id);
        // A constant materializes to itself.
        if instance
            .clone()
            .as_interface::<dyn ConstantLike>()
            .is_some()
        {
            return Ok(());
        }
        // A read leaves memory as it found it, so the state it publishes is the
        // state it read: its uses reroute to that operand and the read goes with
        // its value. Nothing else multi-result names one value to replace.
        let state_edge = instance
            .clone()
            .as_interface::<dyn MemoryRead>()
            .and_then(|read| match (read.state_operand(), read.state_result()) {
                (Some(operand), Some(result)) => Some((read.read_value(), operand, result)),
                _ => None,
            });
        let value = match (state_edge, &instance.results[..]) {
            (Some((value, ..)), _) => value,
            (None, &[value]) => value,
            _ => return Ok(()),
        };
        let Some(&class) = self.value_class.get(&value) else {
            return Ok(());
        };
        let ty = self.context.get_value(value).ty();
        let block = instance.parent_block().map(|b| self.context.get_block(b));
        let target = OperationRef::new(instance.clone(), block, None);
        let mut memo = HashMap::new();
        let Some(new_value) =
            self.materialize(extraction, class, ty, &target, rewriter, &mut memo)?
        else {
            return Ok(());
        };
        // The replacement must dominate the use it takes over. Operand reuse and
        // freshly built ops satisfy this by construction; a cross-block CSE or a gate
        // collapsing to an arm may not, so check before committing.
        if new_value != value && self.dominates(new_value, value) {
            self.context.replace_value_uses(value, new_value);
            if let Some((_, operand, result)) = state_edge {
                self.context.replace_value_uses(result, operand);
            }
            // Only erase a pure value op; an op with regions may have side effects
            // whose result merely became unused (left for DCE).
            if instance.regions.is_empty() {
                rewriter.erase_op(&target)?;
            }
        }
        Ok(())
    }

    /// Whether the def of `a` dominates the def of `b`. `b` is always an op result,
    /// so it has a defining op; `a` may be a block argument, located via `arg_block`.
    fn dominates(&self, a: ValueId, b: ValueId) -> bool {
        let (Some(ab), Some(bb)) = (self.def_block(a), self.def_block(b)) else {
            return false;
        };
        if ab != bb {
            return self.dom.dominates(ab, bb) && self.reaches_into(a, ab, bb);
        }
        match (
            self.context.get_value(a).defining_op(),
            self.context.get_value(b).defining_op(),
        ) {
            (Some(a_op), Some(b_op)) => self.context.get_block(ab).is_before(a_op, b_op),
            // A block argument precedes every op in its block.
            (None, _) => true,
            (Some(_), None) => false,
        }
    }

    /// Whether the def of `value` dominates the operation `op` — what a value an
    /// arm yields must do for the terminator that yields it.
    fn dominates_op(&self, value: ValueId, op: OpId) -> bool {
        let (Some(vb), Some(ob)) = (
            self.def_block(value),
            self.context.get_op(op).parent_block(),
        ) else {
            return false;
        };
        if vb != ob {
            return self.dom.dominates(vb, ob) && self.reaches_into(value, vb, ob);
        }
        match self.context.get_value(value).defining_op() {
            Some(def) => self.context.get_block(vb).is_before(def, op),
            // A block argument precedes every op in its block.
            None => true,
        }
    }

    /// Whether `a`, defined in `ab`, is in scope in `bb`. A block dominates the
    /// blocks of the regions its operations hold, but only the part of it that
    /// runs before the holding operation reaches inside, so `a` must precede that
    /// operation. Vacuously true when `bb` is not nested under `ab`.
    fn reaches_into(&self, a: ValueId, ab: BlockId, bb: BlockId) -> bool {
        let Some(holder) = self.holder_in(ab, bb) else {
            return true;
        };
        match self.context.get_value(a).defining_op() {
            Some(a_op) => self.context.get_block(ab).is_before(a_op, holder),
            // A block argument precedes every op in its block.
            None => true,
        }
    }

    /// The operation of `block` whose regions transitively contain `inner`.
    fn holder_in(&self, block: BlockId, inner: BlockId) -> Option<OpId> {
        let mut current = inner;
        loop {
            let region = self.context.parent_region(current)?;
            let holder = self.context.get_region(region).parent_op()?;
            let parent = self.context.get_op(holder).parent_block()?;
            if parent == block {
                return Some(holder);
            }
            current = parent;
        }
    }

    fn def_block(&self, value: ValueId) -> Option<BlockId> {
        match self.context.get_value(value).defining_op() {
            Some(op) => self.context.get_op(op).parent_block(),
            None => self.arg_block.borrow().get(&value).copied(),
        }
    }

    /// Recurse into each nested region, assuming a guard's fact inside its region.
    fn recurse(&mut self, op_ids: &[OpId], rewriter: &mut Rewriter) -> Result<(), PassError> {
        for &op_id in op_ids {
            if !self.context.has_operation(op_id) {
                continue;
            }
            let instance = self.context.get_op(op_id);
            if instance.regions.is_empty() {
                continue;
            }
            let guarded = instance
                .clone()
                .as_interface::<dyn Conditional>()
                .map(|g| g.guarded_regions())
                .unwrap_or_default();
            for &sub in &instance.regions {
                match guarded.iter().find(|&&(r, ..)| r == sub) {
                    Some(&(_, value, holds)) => {
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

    /// Assume `value == holds` in the current context by unioning its class with the
    /// matching boolean constant.
    fn inject(&mut self, value: ValueId, holds: bool) {
        let cond = self
            .value_class
            .get(&value)
            .copied()
            .unwrap_or_else(|| self.eg.add(Node::input(value)));
        let constant = self
            .eg
            .add(Node::constant(APInt::new(1, holds as u64), Prov::None));
        self.eg.union(cond, constant);
        self.eg.rebuild();
    }

    /// Rebuild the value of `class`'s cheapest node: an existing value is reused, a
    /// constant or rule-introduced op is built before `target`. Memoized per class.
    ///
    /// `None` where the term the extraction chose has no value at `target` — a
    /// class no cost model could spell, a gate whose arms do not answer. The
    /// rewrite is then skipped and the operation it would have replaced stays.
    fn materialize(
        &self,
        extraction: &Extraction<Node>,
        class: Id,
        expected_ty: TypeId,
        target: &OperationRef,
        rewriter: &mut Rewriter,
        memo: &mut HashMap<Id, ValueId>,
    ) -> Result<Option<ValueId>, PassError> {
        let class = self.eg.find(class);
        if let Some(&value) = memo.get(&class) {
            return Ok(Some(value));
        }
        if let Some(value) = self.bound_port(class) {
            return Ok(Some(value));
        }
        let Some(node) = self.chosen(extraction, class) else {
            return Ok(None);
        };
        // Provenance decides how a term becomes IR again: a gate stands for its
        // block-argument value, a seeded op or constant for the op that already
        // computes it, a rule-introduced op for its emitter, and a constant no op
        // holds is built here. A law-introduced gate or access names the operation
        // that rebuilds it: the gate grows a port, the access is copied.
        let value = match (node.sym(), node.prov) {
            (_, Prov::Value(value)) => value,
            (Some(SymKind::If), Prov::Op(gate)) => {
                let Some(value) =
                    self.commit_gamma_port(extraction, gate, node, expected_ty, rewriter)?
                else {
                    return Ok(None);
                };
                value
            }
            (Some(SymKind::Theta), Prov::Op(_)) => {
                let Some(value) =
                    self.commit_theta_port(extraction, class, node, expected_ty, target, rewriter)?
                else {
                    return Ok(None);
                };
                value
            }
            (Some(SymKind::LoadMemory), Prov::Op(load)) => {
                let Some(value) = self.reread(extraction, load, node, target, rewriter)? else {
                    return Ok(None);
                };
                value
            }
            (_, Prov::Op(op)) => self.context.get_op(op).results[0],
            (_, Prov::Introduced(idx)) => {
                let ty = node.ty.expect("an op node carries its result type");
                let mut operands = Vec::with_capacity(node.children.len());
                for &arg in &node.children {
                    let Some(operand) =
                        self.materialize(extraction, arg, ty, target, rewriter, memo)?
                    else {
                        return Ok(None);
                    };
                    operands.push(operand);
                }
                let emit = self.ruleset.emits[idx]
                    .as_ref()
                    .expect("an introduced op supplies an emit");
                emit(self.context, &operands, ty, target, rewriter)?
            }
            (_, Prov::None) => {
                let Some(literal) = node.int() else {
                    return Ok(None);
                };
                let op = ops::constant(self.context, literal.to_i64(), expected_ty).build();
                rewriter.insert_op_before(target, &op)?;
                op.result()
            }
        };
        memo.insert(class, value);
        Ok(Some(value))
    }

    /// Carry a law-introduced gate's value out of `gate` on a port of its own.
    ///
    /// Each arm yields the value of the class the gate chose there, materialized
    /// at that arm's terminator, so the port is wired the one way
    /// [`Context::grow_port`] keeps results, arguments and yields consistent. An
    /// arm that cannot answer — no value in scope there — leaves the gate as it
    /// was. One port per gate and pair of arm classes, however many reads it
    /// answers.
    fn commit_gamma_port(
        &self,
        extraction: &Extraction<Node>,
        gate: OpId,
        node: &Node,
        ty: TypeId,
        rewriter: &mut Rewriter,
    ) -> Result<Option<ValueId>, PassError> {
        let [_, taken, not_taken] = node.children[..] else {
            return Ok(None);
        };
        let key = (gate, self.eg.find(taken), self.eg.find(not_taken));
        if let Some(&value) = self.gamma_ports.borrow().get(&key) {
            return Ok(Some(value));
        }
        if !self.context.has_operation(gate) {
            return Ok(None);
        }
        let instance = self.context.get_op(gate);
        let Some(conditional) = instance.clone().as_interface::<dyn Conditional>() else {
            return Ok(None);
        };
        let mut yielded: Vec<(RegionId, ValueId)> = Vec::new();
        for (region, _, when_true) in conditional.guarded_regions() {
            let class = if when_true { taken } else { not_taken };
            let Some(terminator) = self.terminator(region) else {
                return Ok(None);
            };
            let target = self.at(terminator);
            let mut memo = HashMap::new();
            let Some(value) =
                self.materialize(extraction, class, ty, &target, rewriter, &mut memo)?
            else {
                return Ok(None);
            };
            if !self.dominates_op(value, terminator) {
                return Ok(None);
            }
            yielded.push((region, value));
        }
        let result = self.context.grow_port(gate, ty, None, |region, _| {
            yielded
                .iter()
                .find(|&&(carried, _)| carried == region)
                .map(|&(_, value)| value)
        });
        self.gamma_ports.borrow_mut().insert(key, result);
        Ok(Some(result))
    }

    /// The term to rebuild `class` from: the cheapest form the cost model found,
    /// unless a law answered the class with a θ.
    ///
    /// A θ's latch is a term over the port itself, so its cost is a fixpoint over
    /// the very class being extracted and no bottom-up model can prefer it,
    /// however cheap the port is. In the IR that self-reference is a block
    /// argument and costs nothing, so a θ a law introduced is the answer.
    fn chosen<'a>(&'a self, extraction: &'a Extraction<Node>, class: Id) -> Option<&'a Node> {
        self.eg
            .nodes(class)
            .iter()
            .find(|node| node.sym() == Some(SymKind::Theta) && matches!(node.prov, Prov::Op(_)))
            .or_else(|| extraction.node(class))
    }

    /// The port argument a θ class stands for, while that port's latch is built.
    fn bound_port(&self, class: Id) -> Option<ValueId> {
        self.port_bindings
            .borrow()
            .iter()
            .rev()
            .find(|&&(bound, _)| bound == class)
            .map(|&(_, value)| value)
    }

    /// Carry a law-introduced θ's value on a port of `loop_op`'s own.
    ///
    /// The port starts at what the class the loop was entered with materializes
    /// to ahead of the loop; every region of the loop reads it as one more
    /// argument, which a region that only tests the carried values forwards on;
    /// and the body yields what the latch class materializes to at its
    /// terminator, under a binding resolving the θ itself to that argument —
    /// inside the body the carry *is* the argument. What the port is worth at
    /// `target` is then the argument of the region holding it, or the loop's
    /// result outside them all.
    fn commit_theta_port(
        &self,
        extraction: &Extraction<Node>,
        class: Id,
        node: &Node,
        ty: TypeId,
        target: &OperationRef,
        rewriter: &mut Rewriter,
    ) -> Result<Option<ValueId>, PassError> {
        let (Prov::Op(loop_op), &[init_class, latch_class]) = (node.prov, &node.children[..])
        else {
            return Ok(None);
        };
        let key = (loop_op, self.eg.find(init_class), self.eg.find(latch_class));
        if let Some(port) = self.theta_ports.borrow().get(&key) {
            return Ok(Some(self.port_value(port, target)));
        }
        if !self.context.has_operation(loop_op) {
            return Ok(None);
        }
        let instance = self.context.get_op(loop_op);
        let Some(scope) = instance.clone().as_interface::<dyn TokenScope>() else {
            return Ok(None);
        };
        let body = scope.token_scope_regions();
        let at_loop = self.at(loop_op);
        let mut memo = HashMap::new();
        let Some(init) =
            self.materialize(extraction, init_class, ty, &at_loop, rewriter, &mut memo)?
        else {
            return Ok(None);
        };
        if !self.dominates_op(init, loop_op) {
            return Ok(None);
        }
        let mut arguments = Vec::new();
        let mut latched = true;
        let mut failure = None;
        let result = self
            .context
            .grow_port(loop_op, ty, Some(init), |region, incoming| {
                let incoming = incoming?;
                arguments.push((region, incoming));
                if let Some(entry) = self.context.get_region(region).block_ids().first() {
                    self.arg_block.borrow_mut().insert(incoming, *entry);
                }
                if !body.contains(&region) {
                    return Some(incoming);
                }
                let terminator = self.terminator(region)?;
                let at_yield = self.at(terminator);
                self.port_bindings.borrow_mut().push((class, incoming));
                let latch = self.materialize(
                    extraction,
                    latch_class,
                    ty,
                    &at_yield,
                    rewriter,
                    &mut HashMap::new(),
                );
                self.port_bindings.borrow_mut().pop();
                match latch {
                    Ok(Some(value)) if self.dominates_op(value, terminator) => Some(value),
                    // The port is already half grown and no edit can be taken back,
                    // so it carries its own value on: an unread port, and the reads
                    // it would have answered stay as they were.
                    other => {
                        failure = other.err();
                        latched = false;
                        Some(incoming)
                    }
                }
            });
        if let Some(error) = failure {
            return Err(error);
        }
        if !latched {
            return Ok(None);
        }
        let port = ThetaPort { arguments, result };
        let value = self.port_value(&port, target);
        self.theta_ports.borrow_mut().insert(key, port);
        Ok(Some(value))
    }

    /// What `port` is worth where `target` sits: the argument of the loop region
    /// holding it, or the loop's result outside every one of them.
    fn port_value(&self, port: &ThetaPort, target: &OperationRef) -> ValueId {
        let mut block = self.context.get_op(target.op().id).parent_block();
        while let Some(current) = block {
            let Some(region) = self.context.parent_region(current) else {
                break;
            };
            if let Some(&(_, argument)) = port
                .arguments
                .iter()
                .find(|&&(holding, _)| holding == region)
            {
                return argument;
            }
            block = self
                .context
                .get_region(region)
                .parent_op()
                .and_then(|op| self.context.get_op(op).parent_block());
        }
        port.result
    }

    /// A read a law distributed into an arm that stored nothing: with no value
    /// defined there and no operation naming an indeterminate one, the read
    /// itself is the value — a copy of the load being distributed, on the state
    /// that reaches the arm.
    fn reread(
        &self,
        extraction: &Extraction<Node>,
        load: OpId,
        node: &Node,
        target: &OperationRef,
        rewriter: &mut Rewriter,
    ) -> Result<Option<ValueId>, PassError> {
        if !self.context.has_operation(load) {
            return Ok(None);
        }
        let template = self.context.get_op(load);
        let Some(read) = template.clone().as_interface::<dyn MemoryRead>() else {
            return Ok(None);
        };
        let Some(observed) = read.state_operand() else {
            return Ok(None);
        };
        // The copy splices into the linear chain just before `target`, so the
        // state it reads is the one `target` consumes for that chain, and what
        // the copy publishes takes its place. Naming the chain's class elsewhere
        // would read a state this point does not hold.
        let Some(state) = self.consumed_state(target, node.children[state::LOAD_STATE]) else {
            return Ok(None);
        };
        let mut memo = HashMap::new();
        let location = read.read_location();
        let address_ty = self.context.get_value(location).ty();
        let Some(address) = self.materialize(
            extraction,
            node.children[state::ADDRESS],
            address_ty,
            target,
            rewriter,
            &mut memo,
        )?
        else {
            return Ok(None);
        };
        let results: Vec<ValueId> = template
            .results
            .iter()
            .map(|&result| {
                self.context
                    .create_value(self.context.get_value(result).ty(), None)
                    .id()
            })
            .collect();
        if let Some(published) = read.state_result()
            && let Some(index) = template.results.iter().position(|&r| r == published)
        {
            self.context.replace_value_uses(state, results[index]);
        }
        let operands = template
            .operands
            .iter()
            .map(|&operand| match operand {
                _ if operand == location => address,
                _ if operand == observed => state,
                _ => operand,
            })
            .collect();
        let copy = self.context.add_operation(OpInstance::new_dynamic(
            (template.dialect().as_str(), template.name().as_str()),
            self.context.as_context_ref(),
            operands,
            results.clone(),
            vec![],
            template.attributes.clone(),
        ));
        rewriter.insert_op_before(target, copy.as_dyn_op().as_ref())?;
        Ok(Some(results[0]))
    }

    /// The operand of `target` naming the chain `class` is: the state reaching
    /// the point just before it.
    fn consumed_state(&self, target: &OperationRef, class: Id) -> Option<ValueId> {
        let class = self.eg.find(class);
        self.context
            .get_op(target.op().id)
            .operands
            .iter()
            .copied()
            .find(|operand| {
                self.value_class
                    .get(operand)
                    .is_some_and(|&named| self.eg.find(named) == class)
            })
    }

    /// A cursor at `op`, for a rewrite to build in front of.
    fn at(&self, op: OpId) -> OperationRef {
        let instance = self.context.get_op(op);
        let block = instance.parent_block().map(|id| self.context.get_block(id));
        OperationRef::new(instance, block, None)
    }

    /// The terminator of `region`, when it is the single-block region a port can
    /// be grown on.
    fn terminator(&self, region: RegionId) -> Option<OpId> {
        let mut blocks = self.context.get_region(region).iter(self.context.clone());
        let block = blocks.next()?;
        if blocks.next().is_some() {
            return None;
        }
        block.op_ids().last().copied()
    }
}
