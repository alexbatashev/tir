use std::collections::{HashMap, HashSet};

use crate::{
    BlockId, BranchGuard, BranchTerminator, Conditional, Context, LoopLike, OpId, Terminator,
    TokenScope, ValueId,
    analysis::{Analysis, AnalysisManager, DefUse, DominatorTree},
    builtin::TokenType,
    graph::{Dag, GenericDag, MutDag, NodeId},
};

/// A node of the gated SSA value graph. Each models the producer of one value and
/// references existing IR by id; its inputs are the node's out-edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GateNode {
    /// A value computed by an existing operation, referenced by [`OpId`]. Edges go
    /// to the producers of the operation's operands, in operand order.
    Op(OpId),
    /// A value entering the region from outside (function/entry-block argument, or
    /// a definition not modelled here). A leaf with no edges.
    Input(ValueId),
    /// γ gate replacing a non-loop block-argument phi. Edges are
    /// `[condition, true_input, false_input]`: the value taken when `cond` is
    /// true/false respectively.
    Gamma { value: ValueId, cond: ValueId },
    /// μ gate replacing a loop-header block-argument phi. Edges are
    /// `[init, latch]`: the pre-loop input and the value latched on the back edge.
    Mu { value: ValueId },
    /// η gate for a value leaving a structured loop: the single edge goes to the μ of
    /// the carried value. It exists so the post-loop value is not identified with the
    /// in-loop one — they are equal only on the final iteration. The loop-termination
    /// predicate is not an edge: `scf.for`'s is implicit in its bounds, so an η carries
    /// no condition rather than carrying one for some loops and not others.
    Eta { value: ValueId },
    /// An n-way phi that could not be reduced to a γ/μ gate. Edges are the incoming
    /// values in predecessor order.
    Phi { value: ValueId },
}

/// Gated SSA for the blocks reachable from a root operation's first region.
pub struct GSA {
    inner: GenericDag<GateNode, ()>,
    nodes: HashMap<ValueId, NodeId>,
    gate_inputs: HashMap<ValueId, Vec<ValueId>>,
}

impl GSA {
    /// Build the gated SSA form rooted at `root`'s first region.
    pub fn new<O: Into<OpId>>(context: &Context, root: O) -> Self {
        let root = root.into();
        Self::with_dom(context, root, &DominatorTree::new(context, root))
    }

    fn with_dom(context: &Context, root: OpId, dom: &DominatorTree) -> Self {
        let blocks = region_blocks(context, root);
        let preds = predecessor_map(context, &blocks);
        let phis = collect_phis(context, &blocks, &preds);
        let StructuredGates {
            gamma,
            mu,
            loop_result,
        } = structured_gates(context, root, &blocks);

        let mut builder = Builder {
            context,
            dom,
            preds,
            phis,
            gamma,
            mu,
            loop_result,
            inner: GenericDag::new(),
            nodes: HashMap::new(),
            gate_inputs: HashMap::new(),
        };

        // Materialize a node for every value defined in the region; recursion pulls
        // in operands and gate inputs.
        for &block in &blocks {
            let blk = context.get_block(block);
            for arg in blk.arguments() {
                builder.node_for_value(arg.id());
            }
            for op in blk.op_ids() {
                for &result in &context.get_op(op).results {
                    builder.node_for_value(result);
                }
            }
        }

        Self {
            inner: builder.inner,
            nodes: builder.nodes,
            gate_inputs: builder.gate_inputs,
        }
    }

    /// The node producing `value`, if it is part of the form.
    pub fn node_of(&self, value: ValueId) -> Option<NodeId> {
        self.nodes.get(&value).copied()
    }

    /// The gate at `id`.
    pub fn gate(&self, id: NodeId) -> &GateNode {
        self.inner.get_node(id)
    }

    /// The value produced by `id`, or `None` for an [`GateNode::Op`] node (an op may
    /// have several results, so no single value identifies it).
    pub fn value_of(&self, id: NodeId) -> Option<ValueId> {
        match self.inner.get_node(id) {
            GateNode::Op(_) => None,
            GateNode::Input(v)
            | GateNode::Gamma { value: v, .. }
            | GateNode::Mu { value: v }
            | GateNode::Eta { value: v }
            | GateNode::Phi { value: v } => Some(*v),
        }
    }

    /// The operation referenced by `id`, if it is an [`GateNode::Op`] node.
    pub fn op_of(&self, id: NodeId) -> Option<OpId> {
        match self.inner.get_node(id) {
            GateNode::Op(op) => Some(*op),
            _ => None,
        }
    }

    /// IR values feeding `value`'s gated-SSA node, in edge order.
    pub fn inputs_of(&self, value: ValueId) -> Option<&[ValueId]> {
        self.gate_inputs.get(&value).map(Vec::as_slice)
    }
}

impl Analysis for GSA {
    fn build(analyses: &AnalysisManager, context: &Context, op: OpId) -> Self {
        Self::with_dom(context, op, &analyses.get::<DominatorTree>(context, op))
    }
}

impl Dag for GSA {
    type Node = GateNode;
    type Leaf = ();
    type Annotation = ();

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn get_node(&self, id: NodeId) -> &Self::Node {
        self.inner.get_node(id)
    }

    fn get_leaf_data(&self, id: NodeId) -> Option<&Self::Leaf> {
        self.inner.get_leaf_data(id)
    }

    fn get_annotation(&self, id: NodeId) -> Option<&Self::Annotation> {
        self.inner.get_annotation(id)
    }

    fn root(&self) -> Option<NodeId> {
        self.inner.root()
    }

    fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> {
        self.inner.children(id)
    }

    fn postorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        self.inner.postorder(start)
    }

    fn preorder(&self, start: NodeId) -> impl Iterator<Item = NodeId> {
        self.inner.preorder(start)
    }
}

/// One incoming control-flow edge of a block: the predecessor and the values it
/// forwards to the block's arguments, in argument order.
struct Edge {
    pred: BlockId,
    args: Vec<ValueId>,
}

struct Builder<'a> {
    context: &'a Context,
    dom: &'a DominatorTree,
    /// Block-SSA predecessors reaching each block, with the values forwarded along
    /// the edge.
    preds: HashMap<BlockId, Vec<Edge>>,
    /// Block arguments that are phis (their block has branch predecessors), mapped to
    /// their block and argument index.
    phis: HashMap<ValueId, (BlockId, usize)>,
    /// Result of a structured conditional ([`Conditional`]) → `(cond, true, false)` γ
    /// inputs.
    gamma: HashMap<ValueId, (ValueId, ValueId, ValueId)>,
    /// Loop-carried region argument ([`LoopCarried`]) → `(init, latch)` μ inputs.
    mu: HashMap<ValueId, (ValueId, ValueId)>,
    /// Structured loop result → its carried region argument, the η gate's input.
    loop_result: HashMap<ValueId, ValueId>,
    inner: GenericDag<GateNode, ()>,
    nodes: HashMap<ValueId, NodeId>,
    gate_inputs: HashMap<ValueId, Vec<ValueId>>,
}

impl Builder<'_> {
    /// The node producing `value`, materializing it (and, transitively, its inputs)
    /// on first request. The node is memoized before its edges are added so back
    /// edges through μ gates resolve to it instead of recursing forever.
    fn node_for_value(&mut self, value: ValueId) -> NodeId {
        if let Some(&node) = self.nodes.get(&value) {
            return node;
        }

        if let Some((gate, inputs)) = self.gate_plan(value) {
            let node = self.inner.add_node(gate);
            self.nodes.insert(value, node);
            self.gate_inputs.insert(value, inputs.clone());
            for input in inputs {
                let child = self.node_for_value(input);
                self.inner.add_edge(node, child);
            }
            return node;
        }

        if let Some(op) = self.context.get_value(value).defining_op() {
            let node = self.inner.add_node(GateNode::Op(op));
            self.nodes.insert(value, node);
            for &operand in &self.context.get_op(op).operands {
                let child = self.node_for_value(operand);
                self.inner.add_edge(node, child);
            }
            return node;
        }

        let node = self.inner.add_node(GateNode::Input(value));
        self.nodes.insert(value, node);
        node
    }

    /// The gate replacing `value` and the inputs feeding it, in edge order, if `value`
    /// is an unstructured phi, a structured γ result, a structured μ carried value, or
    /// a structured loop result (η).
    fn gate_plan(&self, value: ValueId) -> Option<(GateNode, Vec<ValueId>)> {
        if let Some(&(block, index)) = self.phis.get(&value) {
            return Some(self.plan_gate(block, index, value));
        }
        if let Some(&(cond, t, f)) = self.gamma.get(&value) {
            return Some((GateNode::Gamma { value, cond }, vec![cond, t, f]));
        }
        if let Some(&(init, latch)) = self.mu.get(&value) {
            return Some((GateNode::Mu { value }, vec![init, latch]));
        }
        if let Some(&carried) = self.loop_result.get(&value) {
            return Some((GateNode::Eta { value }, vec![carried]));
        }
        None
    }

    /// Classify the phi `value` (argument `index` of `block`) into a gate and list
    /// its inputs in edge order.
    fn plan_gate(&self, block: BlockId, index: usize, value: ValueId) -> (GateNode, Vec<ValueId>) {
        let incoming = self.incoming(block, index);
        let phi = || {
            (
                GateNode::Phi { value },
                incoming.iter().map(|&(_, v)| v).collect(),
            )
        };

        if incoming
            .iter()
            .any(|&(pred, _)| self.dom.dominates(block, pred))
        {
            // A loop header. A μ holds one init and one latch, so any other shape
            // (two back edges, two entry edges) would have to drop an input — stay an
            // opaque phi instead of claiming a value the block does not have.
            return self.mu_gate(block, value, &incoming).unwrap_or_else(phi);
        }
        if let Some(gate) = self.gamma_gate(block, value, &incoming) {
            return gate;
        }

        phi()
    }

    /// μ gate for a loop header: exactly one incoming edge comes from a block the
    /// header dominates (the back edge) and exactly one does not. Inputs are
    /// `[init, latch]`.
    fn mu_gate(
        &self,
        block: BlockId,
        value: ValueId,
        incoming: &[(BlockId, ValueId)],
    ) -> Option<(GateNode, Vec<ValueId>)> {
        let mut init = None;
        let mut latch = None;
        for &(pred, v) in incoming {
            let slot = if self.dom.dominates(block, pred) {
                &mut latch
            } else {
                &mut init
            };
            if slot.replace(v).is_some() {
                return None;
            }
        }
        Some((GateNode::Mu { value }, vec![init?, latch?]))
    }

    /// γ gate for a two-way merge: the immediately dominating terminator is a
    /// [`BranchGuard`], and each incoming edge is reached through one of its guarded
    /// successors. Inputs are `[condition, true_input, false_input]`.
    fn gamma_gate(
        &self,
        block: BlockId,
        value: ValueId,
        incoming: &[(BlockId, ValueId)],
    ) -> Option<(GateNode, Vec<ValueId>)> {
        if incoming.len() != 2 {
            return None;
        }
        let idom = self.dom.idom(block)?;
        let term = self.context.get_block(idom).op_ids().last().copied()?;
        let guarded = self
            .context
            .get_op(term)
            .as_interface::<dyn BranchGuard>()?
            .guarded_successors();

        let mut cond = None;
        let mut true_val = None;
        let mut false_val = None;
        for &(pred, v) in incoming {
            for &(succ, c, taken_when_true) in &guarded {
                if self.dom.dominates(succ, pred) {
                    cond = Some(c);
                    if taken_when_true {
                        true_val.get_or_insert(v);
                    } else {
                        false_val.get_or_insert(v);
                    }
                    break;
                }
            }
        }
        match (cond, true_val, false_val) {
            (Some(cond), Some(t), Some(f)) => {
                Some((GateNode::Gamma { value, cond }, vec![cond, t, f]))
            }
            _ => None,
        }
    }

    /// The `(predecessor, forwarded value)` pairs feeding argument `index` of `block`.
    fn incoming(&self, block: BlockId, index: usize) -> Vec<(BlockId, ValueId)> {
        self.preds
            .get(&block)
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.args.get(index).map(|&v| (edge.pred, v)))
            .collect()
    }
}

/// Every block reachable from `root`'s first region, descending into nested regions
/// just as the dominator-tree CFG builder does.
fn region_blocks(context: &Context, root: OpId) -> Vec<BlockId> {
    let entry = context
        .get_op(root)
        .regions
        .first()
        .and_then(|region| context.get_region(*region).iter(context.clone()).next())
        .map(|block| block.id());

    let mut order = Vec::new();
    let Some(entry) = entry else {
        return order;
    };

    let mut seen = HashSet::new();
    let mut stack = vec![entry];
    seen.insert(entry);

    while let Some(block_id) = stack.pop() {
        order.push(block_id);
        let op_ids = context.get_block(block_id).op_ids();
        let mut edges = Vec::new();

        for op_id in &op_ids {
            for region_id in &context.get_op(*op_id).regions {
                if let Some(child) = context.get_region(*region_id).iter(context.clone()).next() {
                    edges.push(child.id());
                }
            }
        }
        if let Some(&term) = op_ids.last() {
            let instance = context.get_op(term);
            if let Some(terminator) = instance.as_interface::<dyn Terminator>() {
                edges.extend(terminator.successors());
            }
        }

        for target in edges {
            if seen.insert(target) {
                stack.push(target);
            }
        }
    }

    order
}

/// Map each block to its incoming [`BranchTerminator`] edges. Terminators that forward
/// no block arguments (returns, yields, structured-region edges) do not implement the
/// interface and so contribute no predecessors.
fn predecessor_map(context: &Context, blocks: &[BlockId]) -> HashMap<BlockId, Vec<Edge>> {
    let mut preds: HashMap<BlockId, Vec<Edge>> = HashMap::new();
    for &block in blocks {
        let Some(&term) = context.get_block(block).op_ids().last() else {
            continue;
        };
        let Some(branch) = context.get_op(term).as_interface::<dyn BranchTerminator>() else {
            continue;
        };
        for (succ, args) in branch.successor_operands() {
            preds
                .entry(succ)
                .or_default()
                .push(Edge { pred: block, args });
        }
    }
    preds
}

/// The block-argument values that are phis: arguments of any block with branch
/// predecessors, mapped to their block and argument index.
fn collect_phis(
    context: &Context,
    blocks: &[BlockId],
    preds: &HashMap<BlockId, Vec<Edge>>,
) -> HashMap<ValueId, (BlockId, usize)> {
    let mut phis = HashMap::new();
    for &block in blocks {
        if !preds.contains_key(&block) {
            continue;
        }
        for (index, arg) in context.get_block(block).arguments().iter().enumerate() {
            phis.insert(arg.id(), (block, index));
        }
    }
    phis
}

/// Structured-control-flow gates collected from the region's ops.
struct StructuredGates {
    /// γ: result of a [`Conditional`] op → `(cond, true_input, false_input)`.
    gamma: HashMap<ValueId, (ValueId, ValueId, ValueId)>,
    /// μ: carried region argument of a [`LoopCarried`] op → `(init, latch)`.
    mu: HashMap<ValueId, (ValueId, ValueId)>,
    /// Structured loop result → its carried region argument.
    loop_result: HashMap<ValueId, ValueId>,
}

/// Scan the region's ops for structured control flow: a [`Conditional`] op that
/// produces a value yields a γ over its arms' yielded values; a [`LoopCarried`] op
/// yields a μ over its carried region argument, with its result an η over that μ.
fn structured_gates(context: &Context, root: OpId, blocks: &[BlockId]) -> StructuredGates {
    let def_use = DefUse::new(context, root);
    let mut gamma = HashMap::new();
    let mut mu = HashMap::new();
    let mut loop_result = HashMap::new();

    for &block in blocks {
        for op in context.get_block(block).op_ids() {
            let instance = context.get_op(op);
            // A structured gate exists only where the op produces a value: a resultless
            // `scf.if`/loop is side-effecting and carries nothing.
            let results = instance.results.clone();
            if results.is_empty() {
                continue;
            }

            if let Some(guard) = instance.clone().as_interface::<dyn Conditional>() {
                for (index, &result) in results.iter().enumerate() {
                    if let Some(inputs) = gamma_inputs(guard.as_ref(), index) {
                        gamma.insert(result, inputs);
                    }
                }
            } else if let Some(lp) = instance.clone().as_interface::<dyn LoopLike>() {
                // A μ holds one init and one latch and an η one exit, so a loop the body
                // can also leave early would have to drop an input — stay opaque instead
                // of claiming a value the loop does not have.
                if leaves_early(context, &def_use, &instance) {
                    continue;
                }
                let (carried, inits, latched) = (lp.carried_args(), lp.inits(), lp.latched());
                for (port, &result) in results.iter().enumerate() {
                    mu.insert(carried[port], (inits[port], latched[port]));
                    loop_result.insert(result, carried[port]);
                }
            }
        }
    }

    StructuredGates {
        gamma,
        mu,
        loop_result,
    }
}

/// Whether the loop's body can leave other than through its back edge: something names
/// the token its early exits consume.
fn leaves_early(
    context: &Context,
    def_use: &DefUse,
    instance: &std::sync::Arc<crate::OpInstance>,
) -> bool {
    let Some(scope) = instance.clone().as_interface::<dyn TokenScope>() else {
        return false;
    };
    let token = TokenType::new(context);
    scope
        .token_scope_regions()
        .into_iter()
        .flat_map(|region| context.get_region(region).iter(context.clone()))
        .flat_map(|block| block.arguments().to_vec())
        .any(|argument| argument.ty() == token && def_use.is_used(argument.id().number()))
}

/// The γ inputs `(cond, true_input, false_input)` for result `index` of a two-armed
/// [`Conditional`]: the condition and the values its true/false regions yield there.
fn gamma_inputs(guard: &dyn Conditional, index: usize) -> Option<(ValueId, ValueId, ValueId)> {
    let mut true_val = None;
    let mut false_val = None;
    for (region, _, taken_when_true) in guard.guarded_regions() {
        let yielded = guard.region_yields(region).get(index).copied()?;
        if taken_when_true {
            true_val = Some(yielded);
        } else {
            false_val = Some(yielded);
        }
    }
    Some((guard.decision(), true_val?, false_val?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Context, Operand, Operation, RegionId, TypeId,
        builtin::{IntegerType, UnitType, ops},
    };

    fn func_with_region(context: &Context, region: RegionId) -> OpId {
        ops::func(context, "f", UnitType::new(context), Some(region))
            .build()
            .id()
    }

    fn children(gs: &GSA, node: NodeId) -> Vec<NodeId> {
        gs.children(node).collect()
    }

    #[test]
    fn gamma_for_diamond_merge() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let cond = context.create_value(i1, None);
        let cond_id = cond.id();

        let region = context.create_region();
        let entry = context.create_block(vec![cond]);
        let t = context.create_block(vec![]);
        let f = context.create_block(vec![]);
        let merge_arg = context.create_value(i32, None);
        let merge_arg_id = merge_arg.id();
        let merge = context.create_block(vec![merge_arg]);
        for block in [&entry, &t, &f, &merge] {
            region.add_block(block.id());
        }

        entry
            .clone()
            .append_op(ops::cond_br(&context, cond_id, vec![], vec![], t.id(), f.id()).build());

        let c_true = ops::constant(&context, 1, i32).build();
        let true_val = c_true.result();
        let tb = t.clone();
        tb.append_op(c_true);
        tb.append_op(ops::br(&context, vec![true_val], merge.id()).build());

        let c_false = ops::constant(&context, 0, i32).build();
        let false_val = c_false.result();
        f.append_op(c_false);
        f.append_op(ops::br(&context, vec![false_val], merge.id()).build());

        merge
            .clone()
            .append_op(ops::r#return(&context, Operand::from(merge_arg_id)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        let node = gs.node_of(merge_arg_id).unwrap();
        assert_eq!(
            *gs.gate(node),
            GateNode::Gamma {
                value: merge_arg_id,
                cond: cond_id,
            }
        );
        assert_eq!(
            gs.inputs_of(merge_arg_id),
            Some([cond_id, true_val, false_val].as_slice())
        );

        let kids = children(&gs, node);
        assert_eq!(kids.len(), 3);
        // Order is [condition, true_input, false_input].
        assert_eq!(gs.value_of(kids[0]), Some(cond_id));
        assert_eq!(gs.op_of(kids[1]), Some(true_val_op(&context, true_val)));
        assert_eq!(gs.op_of(kids[2]), Some(true_val_op(&context, false_val)));
    }

    fn true_val_op(context: &Context, value: ValueId) -> OpId {
        context.get_value(value).defining_op().unwrap()
    }

    #[test]
    fn mu_for_loop_header() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let cond = context.create_value(i1, None);
        let cond_id = cond.id();

        let region = context.create_region();
        let entry = context.create_block(vec![]);
        let iv = context.create_value(i32, None);
        let iv_id = iv.id();
        let header = context.create_block(vec![iv]);
        let body = context.create_block(vec![]);
        let exit = context.create_block(vec![]);
        for block in [&entry, &header, &body, &exit] {
            region.add_block(block.id());
        }

        let init = ops::constant(&context, 0, i32).build();
        let init_val = init.result();
        let init_op = init.id();
        entry.append_op(init);
        entry.append_op(ops::br(&context, vec![init_val], header.id()).build());

        header.append_op(
            ops::cond_br(&context, cond_id, vec![], vec![], body.id(), exit.id()).build(),
        );

        let step = ops::constant(&context, 1, i32).build();
        let step_val = step.result();
        let next = ops::addi(&context, iv_id, step_val, i32).build();
        let next_val = next.result();
        let next_op = next.id();
        body.append_op(step);
        body.append_op(next);
        body.append_op(ops::br(&context, vec![next_val], header.id()).build());

        exit.clone()
            .append_op(ops::r#return(&context, Operand::from(iv_id)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        let node = gs.node_of(iv_id).unwrap();
        assert_eq!(*gs.gate(node), GateNode::Mu { value: iv_id });
        assert_eq!(gs.inputs_of(iv_id), Some([init_val, next_val].as_slice()));

        let kids = children(&gs, node);
        assert_eq!(kids.len(), 2);
        // Order is [init, latch].
        assert_eq!(gs.op_of(kids[0]), Some(init_op));
        assert_eq!(gs.op_of(kids[1]), Some(next_op));

        // The latch (`addi`) reads the μ node back, closing the loop.
        let latch_inputs = children(&gs, kids[1]);
        assert!(latch_inputs.contains(&node));
    }

    #[test]
    fn two_latches_stay_a_phi() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let cond = context.create_value(i1, None);
        let cond_id = cond.id();

        let region = context.create_region();
        let entry = context.create_block(vec![cond]);
        let iv = context.create_value(i32, None);
        let iv_id = iv.id();
        let header = context.create_block(vec![iv]);
        let body = context.create_block(vec![]);
        let latch_a = context.create_block(vec![]);
        let latch_b = context.create_block(vec![]);
        let exit = context.create_block(vec![]);
        for block in [&entry, &header, &body, &latch_a, &latch_b, &exit] {
            region.add_block(block.id());
        }

        let init = ops::constant(&context, 0, i32).build();
        let init_val = init.result();
        entry.append_op(init);
        entry.append_op(ops::br(&context, vec![init_val], header.id()).build());

        header.append_op(
            ops::cond_br(&context, cond_id, vec![], vec![], body.id(), exit.id()).build(),
        );
        body.append_op(
            ops::cond_br(
                &context,
                cond_id,
                vec![],
                vec![],
                latch_a.id(),
                latch_b.id(),
            )
            .build(),
        );

        let one = ops::constant(&context, 1, i32).build();
        let one_val = one.result();
        let ab = latch_a.clone();
        ab.append_op(one);
        ab.append_op(ops::br(&context, vec![one_val], header.id()).build());

        let two = ops::constant(&context, 2, i32).build();
        let two_val = two.result();
        latch_b.append_op(two);
        latch_b.append_op(ops::br(&context, vec![two_val], header.id()).build());

        exit.clone()
            .append_op(ops::r#return(&context, Operand::from(iv_id)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        // A μ has room for one latch only; with two, the gate must keep every input.
        let node = gs.node_of(iv_id).unwrap();
        assert_eq!(*gs.gate(node), GateNode::Phi { value: iv_id });
        let inputs = gs.inputs_of(iv_id).unwrap();
        assert!(inputs.contains(&init_val));
        assert!(inputs.contains(&one_val));
        assert!(inputs.contains(&two_val));
    }

    #[test]
    fn entry_arguments_are_inputs() {
        let context = Context::with_default_dialects();
        let i32 = IntegerType::new(&context, 32);
        let arg = context.create_value(i32, None);
        let arg_id = arg.id();

        let region = context.create_region();
        let entry = context.create_block(vec![arg]);
        region.add_block(entry.id());
        entry
            .clone()
            .append_op(ops::r#return(&context, Operand::from(arg_id)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        let node = gs.node_of(arg_id).unwrap();
        assert_eq!(*gs.gate(node), GateNode::Input(arg_id));
    }

    #[test]
    fn manager_shares_dominance_with_gsa() {
        let context = Context::with_default_dialects();
        let i32 = IntegerType::new(&context, 32);
        let arg = context.create_value(i32, None);
        let arg_id = arg.id();

        let region = context.create_region();
        let entry = context.create_block(vec![arg]);
        region.add_block(entry.id());
        entry
            .clone()
            .append_op(ops::r#return(&context, Operand::from(arg_id)).build());
        let func = func_with_region(&context, region.id());

        let am = AnalysisManager::new();
        let gs = am.get::<GSA>(&context, func);
        assert!(gs.node_of(arg_id).is_some());
        // Building GSA populated its dominance dependency in the cache.
        assert!(am.get_cached::<DominatorTree>(&context, func).is_some());
    }

    /// A region yielding a single value through its terminator.
    fn yielding_region(context: &Context, value: ValueId, def: impl Operation) -> RegionId {
        let region = context.create_region();
        let block = context.create_block(vec![]);
        region.add_block(block.id());
        let b = block;
        b.append_op(def);
        b.append_op(crate::scf::ops::r#yield(context, vec![value]).build());
        region.id()
    }

    #[test]
    fn gamma_for_scf_if_result() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let cond = context.create_value(i1, None);
        let cond_id = cond.id();

        let c_true = ops::constant(&context, 1, i32).build();
        let true_val = c_true.result();
        let true_op = c_true.id();
        let then_region = yielding_region(&context, true_val, c_true);

        let c_false = ops::constant(&context, 0, i32).build();
        let false_val = c_false.result();
        let false_op = c_false.id();
        let else_region = yielding_region(&context, false_val, c_false);

        let region = context.create_region();
        let entry = context.create_block(vec![cond]);
        region.add_block(entry.id());

        let if_op = crate::scf::ops::r#if(
            &context,
            cond_id,
            vec![],
            vec![i32],
            Some(then_region),
            Some(else_region),
        )
        .build();
        let if_result = if_op.result();
        entry.append_op(if_op);
        entry.append_op(ops::r#return(&context, Operand::from(if_result)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        let node = gs.node_of(if_result).unwrap();
        assert_eq!(
            *gs.gate(node),
            GateNode::Gamma {
                value: if_result,
                cond: cond_id,
            }
        );

        let kids = children(&gs, node);
        assert_eq!(kids.len(), 3);
        // Order is [condition, true_input, false_input].
        assert_eq!(gs.value_of(kids[0]), Some(cond_id));
        assert_eq!(gs.op_of(kids[1]), Some(true_op));
        assert_eq!(gs.op_of(kids[2]), Some(false_op));
    }

    /// A region defining two constants and yielding both. Returns the region and the
    /// two defining op ids, in yield order.
    fn two_value_region(context: &Context, ty: TypeId, values: [i64; 2]) -> (RegionId, Vec<OpId>) {
        let first = ops::constant(context, values[0], ty).build();
        let second = ops::constant(context, values[1], ty).build();
        let yielded = vec![first.result(), second.result()];
        let defs = vec![first.id(), second.id()];
        let region = context.create_region();
        let block = context.create_block(vec![]);
        region.add_block(block.id());
        let b = block;
        b.append_op(first);
        b.append_op(second);
        b.append_op(crate::scf::ops::r#yield(context, yielded).build());
        (region.id(), defs)
    }

    #[test]
    fn gamma_per_result_of_multi_result_if() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let cond = context.create_value(i1, None);
        let cond_id = cond.id();

        let (then_region, then_defs) = two_value_region(&context, i32, [1, 2]);
        let (else_region, else_defs) = two_value_region(&context, i32, [3, 4]);

        let region = context.create_region();
        let entry = context.create_block(vec![cond]);
        region.add_block(entry.id());

        let if_op = crate::scf::ops::r#if(
            &context,
            cond_id,
            vec![],
            vec![i32, i32],
            Some(then_region),
            Some(else_region),
        )
        .build();
        let results = context.get_op(if_op.id()).results.clone();
        entry.append_op(if_op);
        entry.append_op(ops::r#return(&context, Operand::from(results[0])).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        // Every result of the conditional gets its own γ over that result's arm values.
        for (index, &result) in results.iter().enumerate() {
            let node = gs.node_of(result).unwrap();
            assert_eq!(
                *gs.gate(node),
                GateNode::Gamma {
                    value: result,
                    cond: cond_id,
                }
            );
            let kids = children(&gs, node);
            assert_eq!(gs.value_of(kids[0]), Some(cond_id));
            assert_eq!(gs.op_of(kids[1]), Some(then_defs[index]));
            assert_eq!(gs.op_of(kids[2]), Some(else_defs[index]));
        }
    }

    /// A loop body carrying `acc`: `%next = addi %acc, 1; scf.yield %next`. Returns the
    /// body region, the carried block argument, and the latch (`addi`) op id.
    fn counting_body(context: &Context, i32: TypeId) -> (RegionId, ValueId, OpId) {
        let acc = context.create_value(i32, None);
        let acc_id = acc.id();
        let region = context.create_region();
        let block = context.create_block(vec![acc]);
        region.add_block(block.id());

        let step = ops::constant(context, 1, i32).build();
        let step_val = step.result();
        let next = ops::addi(context, acc_id, step_val, i32).build();
        let next_val = next.result();
        let next_op = next.id();
        block.append_op(step);
        block.append_op(next);
        block.append_op(crate::scf::ops::r#yield(context, vec![next_val]).build());
        (region.id(), acc_id, next_op)
    }

    /// Assert the loop's result is an η over the carried argument's μ, itself gated over
    /// `[init, latch]` with the latch reading the μ back.
    fn assert_mu(gs: &GSA, result: ValueId, acc_id: ValueId, init_op: OpId, latch_op: OpId) {
        let eta = gs.node_of(result).unwrap();
        assert_eq!(*gs.gate(eta), GateNode::Eta { value: result });
        assert_eq!(gs.inputs_of(result), Some([acc_id].as_slice()));

        let node = gs.node_of(acc_id).unwrap();
        assert_eq!(children(gs, eta), vec![node]);
        assert_eq!(*gs.gate(node), GateNode::Mu { value: acc_id });

        let kids = children(gs, node);
        assert_eq!(kids.len(), 2);
        // Order is [init, latch].
        assert_eq!(gs.op_of(kids[0]), Some(init_op));
        assert_eq!(gs.op_of(kids[1]), Some(latch_op));
        assert!(children(gs, kids[1]).contains(&node));
    }

    #[test]
    fn mu_for_scf_for_result() {
        let context = Context::with_default_dialects();
        let index = crate::builtin::IndexType::new(&context);
        let i32 = IntegerType::new(&context, 32);
        let lb = context.create_value(index, None);
        let ub = context.create_value(index, None);
        let step = context.create_value(index, None);

        let (body, acc_id, latch_op) = counting_body(&context, i32);

        let init = ops::constant(&context, 0, i32).build();
        let init_val = init.result();
        let init_op = init.id();

        let region = context.create_region();
        let entry = context.create_block(vec![]);
        region.add_block(entry.id());

        let for_op = crate::scf::ops::r#for(
            &context,
            lb.id(),
            ub.id(),
            step.id(),
            vec![init_val],
            vec![i32],
            Some(body),
        )
        .build();
        let for_result = for_op.result();
        entry.append_op(init);
        entry.append_op(for_op);
        entry.append_op(ops::r#return(&context, Operand::from(for_result)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));
        assert_mu(&gs, for_result, acc_id, init_op, latch_op);
    }

    #[test]
    fn mu_and_eta_per_carried_port() {
        let context = Context::with_default_dialects();
        let index = crate::builtin::IndexType::new(&context);
        let i32 = IntegerType::new(&context, 32);
        let lb = context.create_value(index, None);
        let ub = context.create_value(index, None);
        let step = context.create_value(index, None);

        // A body carrying two ports: `%next_j = addi %acc_j, 1; scf.yield %next_0, %next_1`.
        let accs = [
            context.create_value(i32, None),
            context.create_value(i32, None),
        ];
        let acc_ids = [accs[0].id(), accs[1].id()];
        let body_region = context.create_region();
        let body_block = context.create_block(accs.to_vec());
        body_region.add_block(body_block.id());
        let one = ops::constant(&context, 1, i32).build();
        let one_val = one.result();
        body_block.append_op(one);
        let latch_ops: Vec<OpId> = acc_ids
            .iter()
            .map(|&acc| {
                let next = ops::addi(&context, acc, one_val, i32).build();
                let id = next.id();
                body_block.append_op(next);
                id
            })
            .collect();
        let latched = latch_ops
            .iter()
            .map(|&op| context.get_op(op).results[0])
            .collect();
        body_block.append_op(crate::scf::ops::r#yield(&context, latched).build());

        let inits: Vec<_> = [0, 10]
            .iter()
            .map(|&value| ops::constant(&context, value, i32).build())
            .collect();
        let init_ops: Vec<OpId> = inits.iter().map(|op| op.id()).collect();
        let init_vals: Vec<ValueId> = inits.iter().map(|op| op.result()).collect();

        let region = context.create_region();
        let entry = context.create_block(vec![]);
        region.add_block(entry.id());

        let for_op = crate::scf::ops::r#for(
            &context,
            lb.id(),
            ub.id(),
            step.id(),
            init_vals,
            vec![i32, i32],
            Some(body_region.id()),
        )
        .build();
        let results = context.get_op(for_op.id()).results.clone();
        for init in inits {
            entry.append_op(init);
        }
        entry.append_op(for_op);
        entry.append_op(ops::r#return(&context, Operand::from(results[0])).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));

        // Each carried port gets its own μ, and each result its own η over that μ.
        for port in 0..results.len() {
            assert_mu(
                &gs,
                results[port],
                acc_ids[port],
                init_ops[port],
                latch_ops[port],
            );
        }
    }

    #[test]
    fn mu_for_scf_while_result() {
        let context = Context::with_default_dialects();
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let cond = context.create_value(i1, None);

        let (body, acc_id, latch_op) = counting_body(&context, i32);
        let before = context.create_value(i32, None);
        let condition_region = context.create_region();
        let condition_block = context.create_block(vec![before.clone()]);
        condition_region.add_block(condition_block.id());
        condition_block
            .append_op(crate::scf::ops::condition(&context, cond.id(), vec![before.id()]).build());

        let init = ops::constant(&context, 0, i32).build();
        let init_val = init.result();
        let init_op = init.id();

        let region = context.create_region();
        let entry = context.create_block(vec![]);
        region.add_block(entry.id());

        let while_op = crate::scf::ops::r#while(
            &context,
            vec![init_val],
            vec![i32],
            Some(condition_region.id()),
            Some(body),
        )
        .build();
        let while_result = while_op.result();
        entry.append_op(init);
        entry.append_op(while_op);
        entry.append_op(ops::r#return(&context, Operand::from(while_result)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));
        assert_mu(&gs, while_result, acc_id, init_op, latch_op);
    }

    /// A body that can `break` feeds the carried port on two edges and the result on
    /// two exits, which a μ/η pair cannot hold: the loop stays opaque.
    #[test]
    fn no_mu_for_a_loop_that_can_break() {
        let context = Context::with_default_dialects();
        let index = crate::builtin::IndexType::new(&context);
        let i1 = IntegerType::new(&context, 1);
        let i32 = IntegerType::new(&context, 32);
        let lb = context.create_value(index, None);
        let ub = context.create_value(index, None);
        let step = context.create_value(index, None);

        let scope = context.create_value(crate::builtin::TokenType::new(&context), None);
        let acc = context.create_value(i32, None);
        let (scope_id, acc_id) = (scope.id(), acc.id());
        let body = context.create_region();
        let body_block = context.create_block(vec![scope, acc]);
        body.add_block(body_block.id());

        let one = ops::constant(&context, 1, i32).build();
        let next = ops::addi(&context, acc_id, one.result(), i32).build();
        let next_val = next.result();
        let decision = ops::constant(&context, 1, i1).build();
        let decision_val = decision.result();

        let leave = context.create_region();
        let leave_block = context.create_block(vec![]);
        leave.add_block(leave_block.id());
        leave_block.append_op(crate::scf::ops::r#break(&context, scope_id, vec![next_val]).build());
        let stay = context.create_region();
        let stay_block = context.create_block(vec![]);
        stay.add_block(stay_block.id());
        stay_block.append_op(crate::scf::ops::r#yield(&context, vec![]).build());

        body_block.append_op(one);
        body_block.append_op(next);
        body_block.append_op(decision);
        body_block.append_op(
            crate::scf::ops::r#if(
                &context,
                decision_val,
                vec![],
                vec![],
                Some(leave.id()),
                Some(stay.id()),
            )
            .build(),
        );
        body_block.append_op(crate::scf::ops::r#yield(&context, vec![next_val]).build());

        let init = ops::constant(&context, 0, i32).build();
        let init_val = init.result();
        let region = context.create_region();
        let entry = context.create_block(vec![]);
        region.add_block(entry.id());
        let for_op = crate::scf::ops::r#for(
            &context,
            lb.id(),
            ub.id(),
            step.id(),
            vec![init_val],
            vec![i32],
            Some(body.id()),
        )
        .build();
        let for_result = for_op.result();
        entry.append_op(init);
        entry.append_op(for_op);
        entry.append_op(ops::r#return(&context, Operand::from(for_result)).build());

        let gs = GSA::new(&context, func_with_region(&context, region.id()));
        assert!(!matches!(
            gs.gate(gs.node_of(for_result).unwrap()),
            GateNode::Eta { .. }
        ));
        assert!(!matches!(
            gs.gate(gs.node_of(acc_id).unwrap()),
            GateNode::Mu { .. }
        ));
    }
}
