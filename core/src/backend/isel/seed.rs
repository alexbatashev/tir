//! Seeding selection from the sea view instead of lowering operations one by
//! one ([`super::builder`]).
//!
//! The view renders a *region* of the green layer, and selection runs on a CFG
//! function — the destruction the pipeline performs before the backend. So this
//! reconstructs the region the function's value computations form: one node per
//! operation the view can render, and a region argument for everything else. An
//! anchored origin is exactly what the operation-lowered path spells as an input
//! leaf (a block argument, a call result, an operation with no declared
//! semantics), so the two substrates agree on what is opaque; what they do not
//! agree on is *who says so*, which is the point of the swap.
//!
//! Memory is state, and the reconstructed region has no structure to thread a
//! chain through: the CFG was destroyed before the backend, so what ran before a
//! block is not something the region's linear node order names. So the chains
//! are conservative — one fresh region argument per straight-line run of
//! accesses, cut at every block boundary and at every operation whose effect the
//! reconstruction does not model. Inside a run the order *is* the execution
//! order, so a read reaches the writes that happened on it and nothing else.
//! What the chains never do is order anything: the schedule an emission lands is
//! the block's own op order, and a state edge only names identity.

use std::collections::HashMap;

use tir_symbolic::egraph::Id;

use crate::builtin::{IntegerType, UnitType};
use crate::sea::{Graph, NodeId, Origin, PortType, View, kinds};
use crate::sem::egraph::{SemEGraph, minimal_unsigned_apint};
use crate::sem::{Prov, SemGraph, SemNode, SymKind, template_node};
use crate::{
    AttributeDict, Context, MemoryRead, MemoryWrite, OpId, OpInstance, OperationRef, Terminator,
    TypeId, ValueId,
};

use super::node::{class_is_pure, is_memory_kind};

/// What a view-seeded function hands selection: the e-graph, and the two
/// readings that replace the operation lowering's side tables.
pub(crate) struct ViewSeeding {
    pub(crate) egraph: SemEGraph,
    /// The class of every IR value the function computes or reads.
    pub(crate) value_to_class: HashMap<ValueId, Id>,
    /// The class each rendered operation roots.
    pub(crate) op_roots: HashMap<OpId, Id>,
    /// The IR value each anchored leaf stands for ([`View::anchor`] resolved to
    /// the value the origin was reconstructed from).
    pub(crate) anchors: HashMap<ValueId, ValueId>,
}

/// Render `function`'s value computations as a sea region and seed selection
/// from its view. `None` when the view would not render the function (see the
/// module docs), which leaves it on the operation-lowered path.
pub(crate) fn seed_from_view(
    context: &Context,
    function: &OperationRef,
    layout: Option<&AttributeDict>,
) -> Option<ViewSeeding> {
    let plan = Plan::of(context, function)?;

    let mut graph = Graph::new(IntegerType::new(context, 1), &[]);
    let mut arguments: Vec<PortType> = plan
        .anchored
        .iter()
        .map(|&(_, ty)| PortType::Value(ty))
        .collect();
    let first_chain = arguments.len();
    arguments.resize(first_chain + plan.chains, PortType::State);
    let body = graph.open_region(&arguments);

    let mut origins: HashMap<ValueId, Origin> = plan
        .anchored
        .iter()
        .enumerate()
        .map(|(index, &(value, _))| (value, Origin::argument(body, index as u32)))
        .collect();
    let mut chains: Vec<Origin> = (0..plan.chains)
        .map(|chain| Origin::argument(body, (first_chain + chain) as u32))
        .collect();
    let mut nodes: Vec<(OpId, NodeId)> = Vec::new();
    for rendered in &plan.rendered {
        let op = context.get_op(rendered.op);
        let mut inputs: Vec<Origin> = op
            .operands
            .iter()
            .map(|operand| origins.get(operand).copied())
            .collect::<Option<_>>()?;
        let mut outputs: Vec<PortType> = op
            .results
            .iter()
            .map(|&result| PortType::Value(context.get_value(result).ty()))
            .collect();
        if let Some(chain) = rendered.chain {
            inputs.push(chains[chain]);
            outputs.push(PortType::State);
        }
        let op_type = graph.op_type(op.dialect().as_str(), op.name().as_str());
        let node = graph
            .add_node(op_type, &inputs, &outputs, &[], op.attributes.clone())
            .ok()?;
        for (port, &result) in op.results.iter().enumerate() {
            origins.insert(result, Origin::output(node, port as u32));
        }
        if let Some(chain) = rendered.chain {
            chains[chain] = Origin::output(node, op.results.len() as u32);
        }
        nodes.push((rendered.op, node));
    }
    // The chains leave the region: what a caller does with the memory they name
    // is not something this rendering knows, so no law may drop a write on one.
    graph.close_region(body, &chains).ok()?;
    let function_type = PortType::Value(UnitType::new(context));
    let lambda = graph
        .add_node(kinds::LAMBDA, &[], &[function_type], &[body], Vec::new())
        .ok()?;
    graph.finish(&[Origin::output(lambda, 0)]).ok()?;

    let view = View::build(context, &graph, body, layout);
    let mut value_to_class = HashMap::new();
    for (&value, &origin) in &origins {
        value_to_class.insert(value, view.class(origin)?);
    }
    // An operation roots the class of its first output: the value a read or a
    // pure term computes, and the chain a write produces.
    let op_roots: HashMap<OpId, Id> = nodes
        .iter()
        .map(|&(op_id, node)| Some((op_id, view.class(Origin::output(node, 0))?)))
        .collect::<Option<_>>()?;
    for (rendered, &(op_id, node)) in plan.rendered.iter().zip(&nodes) {
        // An origin the view gave up on is an opaque leaf no rule can root, so
        // the operation behind it would reach the cover with nothing to select.
        if !view.models(Origin::output(node, 0)) {
            return None;
        }
        // Only a memory term is effectful, and the schedule orders it. A pure
        // operation whose semantics say otherwise would reach the cover as a
        // value nothing orders.
        if rendered.chain.is_none() && !class_is_pure(view.egraph(), op_roots[&op_id]) {
            return None;
        }
    }

    let mut anchors = HashMap::new();
    for &(value, _) in &plan.anchored {
        let class = value_to_class[&value];
        for leaf in view.egraph().nodes(class) {
            if let Some(synthetic) = leaf.value()
                && view.anchor(synthetic) == origins.get(&value).copied()
            {
                anchors.insert(synthetic, value);
            }
        }
    }

    let class_types = view.class_types();
    let mut egraph = view.into_egraph();
    type_constant_classes(&mut egraph, &class_types);
    wrap_memory_addresses(&mut egraph);

    Some(ViewSeeding {
        egraph,
        value_to_class,
        op_roots,
        anchors,
    })
}

/// Give every constant class a typed member. The width-dependent axioms read a
/// class's width off a member's type, and a constant term carries none — equal
/// constants share a class whatever type spelled them, so the view records the
/// type beside the graph ([`View::class_types`]) instead of on the label. This
/// seeds that reading back in, which is the form the operation-lowered path
/// interns directly.
fn type_constant_classes(egraph: &mut SemEGraph, class_types: &HashMap<Id, TypeId>) {
    let mut typed = Vec::new();
    for (&class, &ty) in class_types {
        let nodes = egraph.nodes(class);
        if nodes.iter().any(|node| node.ty.is_some()) {
            continue;
        }
        if let Some(value) = nodes.iter().find_map(SemNode::int) {
            typed.push((class, value.clone(), ty));
        }
    }
    for (class, value, ty) in typed {
        let node = egraph.add(SemNode::constant(value, Prov::None).typed(ty));
        egraph.union(class, node);
    }
    egraph.rebuild();
}

/// Record `addr = addr + 0` for every address a memory term reads, which is what
/// makes the targets' base+offset patterns match a bare pointer. Addressing is a
/// selection concern and not the region's — the view spells an access over the
/// address the operation names — so the form the rules need is seeded here, as
/// the operation-lowered path spells it (`SemDagBuilder::zero_offset_address`).
///
/// The equality with the bare address is recorded only where the address is pure,
/// as that path does: an effectful address (a loaded pointer) keeps its effect
/// node as the sole materialization of its class, and the access reads the
/// wrapper as a class of its own instead — which is also what keeps the wrapper
/// legal as a match interior where the effect it reads is shared.
fn wrap_memory_addresses(egraph: &mut SemEGraph) {
    let mut terms: Vec<(Id, SemNode)> = Vec::new();
    for class in egraph.classes() {
        for term in egraph.nodes(class.id()) {
            if term.sym().is_some_and(is_memory_kind) {
                terms.push((class.id(), term.clone()));
            }
        }
    }
    let zero = egraph.add(SemNode::constant(minimal_unsigned_apint(0), Prov::None));
    for (class, term) in terms {
        let address = term.children[0];
        let mut wrapper = template_node(SymKind::Add, None, None);
        wrapper.children = vec![address, zero];
        let wrapper = egraph.add(wrapper);
        if class_is_pure(egraph, address) {
            egraph.union(wrapper, address);
        }
        let mut wrapped = term;
        wrapped.children[0] = wrapper;
        let wrapped = egraph.add(wrapped);
        egraph.union(class, wrapped);
    }
    egraph.rebuild();
}

/// Which operations the view renders as terms, which values it anchors, and how
/// many state chains the region threads.
struct Plan {
    rendered: Vec<Rendered>,
    anchored: Vec<(ValueId, TypeId)>,
    chains: usize,
}

/// An operation the region renders, with the chain it threads — `None` for a
/// pure term, which touches no state at all.
struct Rendered {
    op: OpId,
    chain: Option<usize>,
}

impl Plan {
    /// Walk the function in region order, splitting its operations into the ones
    /// the view renders and the values it anchors (every block argument, and the
    /// results of everything else). A memory access joins the chain in scope; a
    /// block boundary and an operation the rendering does not model cut it, so
    /// the next access starts a fresh one. `None` for a function the view does
    /// not render at all.
    fn of(context: &Context, function: &OperationRef) -> Option<Self> {
        let mut rendered = Vec::new();
        let mut anchored: Vec<(ValueId, TypeId)> = Vec::new();
        let mut chains = 0;
        let anchor = |values: &[ValueId], anchored: &mut Vec<(ValueId, TypeId)>| {
            anchored.extend(
                values
                    .iter()
                    .map(|&value| (value, context.get_value(value).ty())),
            );
        };
        for region_id in &function.op().regions {
            let region = context.get_region(*region_id);
            for block in region.iter(context.clone()) {
                let arguments: Vec<ValueId> = block
                    .arguments()
                    .iter()
                    .map(|argument| argument.id())
                    .collect();
                anchor(&arguments, &mut anchored);
                let mut chain = None;
                for op_id in block.op_ids() {
                    let op = context.get_op(op_id);
                    // A floating-point constant selects only where the target
                    // declares a materializer for it, which the view has no
                    // reading of: rendered it would root a class no rule covers,
                    // anchored it would demand a register nothing defines.
                    if op.is::<crate::builtin::ConstantFOp>() {
                        return None;
                    }
                    if accesses_memory(&op) {
                        // The memory vocabulary names one value at most, so an
                        // access computing more is not one it can spell.
                        if op.results.len() > 1 {
                            return None;
                        }
                        let chain = *chain.get_or_insert_with(|| {
                            chains += 1;
                            chains - 1
                        });
                        rendered.push(Rendered {
                            op: op_id,
                            chain: Some(chain),
                        });
                    } else if renders(&op) {
                        rendered.push(Rendered {
                            op: op_id,
                            chain: None,
                        });
                    } else {
                        anchor(&op.results, &mut anchored);
                        chain = None;
                    }
                }
            }
        }
        Some(Plan {
            rendered,
            anchored,
            chains,
        })
    }
}

fn accesses_memory(op: &std::sync::Arc<OpInstance>) -> bool {
    op.clone().as_interface::<dyn MemoryRead>().is_some()
        || op.clone().as_interface::<dyn MemoryWrite>().is_some()
}

/// Whether the view renders `op` as a term: one value result computed from its
/// operands, and semantics saying what it computes. A terminator is control
/// flow, which the region reconstruction does not model — selection reads it
/// off the IR as it always has.
fn renders(op: &std::sync::Arc<OpInstance>) -> bool {
    if op.results.len() != 1 || op.clone().as_interface::<dyn Terminator>().is_some() {
        return false;
    }
    if op
        .clone()
        .as_interface::<dyn crate::ConstantLike>()
        .is_some()
    {
        return true;
    }
    let mut semantics = SemGraph::new();
    op.clone()
        .as_dyn_op()
        .semantic_expr(&mut semantics)
        .is_some()
}
