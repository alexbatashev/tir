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
//! What the view does not render, this declines: memory is state, and a state
//! edge anchors, so a load would arrive at the cover as a leaf no rule can root.
//! Such a function stays on the operation-lowered path until the view models
//! memory (`docs/design/sea.md`, "MemoryRead/MemoryWrite grow state accessors").

use std::collections::HashMap;

use tir_symbolic::egraph::Id;

use crate::builtin::{IntegerType, UnitType};
use crate::sea::{Graph, NodeId, Origin, PortType, View, kinds};
use crate::sem::egraph::SemEGraph;
use crate::sem::{Prov, SemGraph, SemNode};
use crate::{
    AttributeDict, Context, MemoryRead, MemoryWrite, OpId, OpInstance, OperationRef, Terminator,
    TypeId, ValueId,
};

use super::node::class_is_pure;

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
    let arguments: Vec<PortType> = plan
        .anchored
        .iter()
        .map(|&(_, ty)| PortType::Value(ty))
        .collect();
    let body = graph.open_region(&arguments);

    let mut origins: HashMap<ValueId, Origin> = plan
        .anchored
        .iter()
        .enumerate()
        .map(|(index, &(value, _))| (value, Origin::argument(body, index as u32)))
        .collect();
    let mut nodes: Vec<(OpId, NodeId)> = Vec::new();
    for &op_id in &plan.rendered {
        let op = context.get_op(op_id);
        let inputs: Vec<Origin> = op
            .operands
            .iter()
            .map(|operand| origins.get(operand).copied())
            .collect::<Option<_>>()?;
        let result = op.results[0];
        let ty = context.get_value(result).ty();
        let op_type = graph.op_type(op.dialect().as_str(), op.name().as_str());
        let node = graph
            .add_node(
                op_type,
                &inputs,
                &[PortType::Value(ty)],
                &[],
                op.attributes.clone(),
            )
            .ok()?;
        origins.insert(result, Origin::output(node, 0));
        nodes.push((op_id, node));
    }
    graph.close_region(body, &[]).ok()?;
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
    let op_roots: HashMap<OpId, Id> = nodes
        .iter()
        .map(|&(op_id, node)| Some((op_id, view.class(Origin::output(node, 0))?)))
        .collect::<Option<_>>()?;
    // A rendered class holding an effectful term would reach the cover as a
    // value the schedule no longer orders. Nothing declaring memory gets here,
    // so this is the guard against an op whose semantics say otherwise.
    if op_roots
        .values()
        .any(|&class| !class_is_pure(view.egraph(), class))
    {
        return None;
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

/// Which operations the view renders as terms, and which values it anchors.
struct Plan {
    rendered: Vec<OpId>,
    anchored: Vec<(ValueId, TypeId)>,
}

impl Plan {
    /// Walk the function in region order, splitting its operations into the ones
    /// the view renders and the values it anchors (every block argument, and the
    /// results of everything else). `None` for a function the view does not
    /// render at all.
    fn of(context: &Context, function: &OperationRef) -> Option<Self> {
        let mut rendered = Vec::new();
        let mut anchored: Vec<(ValueId, TypeId)> = Vec::new();
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
                for op_id in block.op_ids() {
                    let op = context.get_op(op_id);
                    if op.clone().as_interface::<dyn MemoryRead>().is_some()
                        || op.clone().as_interface::<dyn MemoryWrite>().is_some()
                    {
                        return None;
                    }
                    // A floating-point constant selects only where the target
                    // declares a materializer for it, which the view has no
                    // reading of: rendered it would root a class no rule covers,
                    // anchored it would demand a register nothing defines.
                    if op.is::<crate::builtin::ConstantFOp>() {
                        return None;
                    }
                    if renders(&op) {
                        rendered.push(op_id);
                    } else {
                        anchor(&op.results, &mut anchored);
                    }
                }
            }
        }
        Some(Plan { rendered, anchored })
    }
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
