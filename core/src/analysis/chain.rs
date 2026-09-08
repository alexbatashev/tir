//! One step back along a memory chain: the state a state carries on from.
//!
//! Where that step lands is [`back`]'s to say; what to make of a merge or of a
//! port is the walker's own fold, since a root, a value and a refusal are
//! different answers to different questions.

use crate::state::{JoinOp, SplitOp};
use crate::{Context, Gamma, OpHandle, OpId, Theta, Value, ValueId};

/// What a state carries on from.
pub enum Step {
    /// The chain opens here: an entry state, an allocation, or a state nothing
    /// in the IR says more about.
    Root,
    /// The one state it carries on from.
    From(ValueId),
    /// The states a merge brings together, the changer's own chain first.
    Merge(Vec<ValueId>),
    /// A state a structured `op` names at `index` of its dependency partition:
    /// the port a region is entered on when `entering`, the state the op left
    /// otherwise. A loop's port is its init on the first iteration alone, so
    /// which one it is stays the walker's question.
    Port {
        op: OpId,
        index: usize,
        entering: bool,
    },
}

/// The state `state` carries on from.
pub fn back(context: &Context, state: ValueId) -> Step {
    if let Some(region) = context.region_of_port(state) {
        let handle = context.get_region(region);
        let index = handle
            .dep_arguments()
            .iter()
            .position(|port| port.id() == state);
        if let (Some(op), Some(index)) = (handle.parent_op(), index) {
            return Step::Port {
                op,
                index,
                entering: true,
            };
        }
    }
    let Some(op) = context
        .get_value(state)
        .defining_op()
        .map(|op| context.get_op(op))
    else {
        return port_incoming(context, state).map_or(Step::Root, Step::From);
    };
    if op.is::<JoinOp>() {
        return Step::Merge(op.dep_operands().to_vec());
    }
    if op.is::<SplitOp>() {
        return match split_source(context, &op, state) {
            Some(source) if source != state => Step::From(source),
            _ => Step::Root,
        };
    }
    if let Some(observed) = super::access_of(&op).and_then(|access| access.state) {
        return Step::From(observed);
    }
    let structured = op.has_interface::<dyn Theta>() || op.has_interface::<dyn Gamma>();
    if structured && let Some(index) = op.dep_results().iter().position(|&left| left == state) {
        return Step::Port {
            op: op.id,
            index,
            entering: false,
        };
    }
    Step::Root
}

/// The state one chain stood at before the effect whose result `split` names
/// again: the effect took the merge of the chains it crosses, in the order the
/// split hands them back, so chain `state` came in on the merge's operand at
/// the same index.
fn split_source(context: &Context, split: &OpHandle, state: ValueId) -> Option<ValueId> {
    let index = split.dep_results().iter().position(|&r| r == state)?;
    let changed = *split.dep_operands().first()?;
    let changer = context.get_op(context.get_value(changed).defining_op()?);
    // The chains a function opens are one entry state split: each is a chain
    // of its own, rooted where the split names it, which the caller reads off
    // the state coming back unchanged.
    let [taken] = changer.dep_operands()[..] else {
        return changer.dep_operands().is_empty().then_some(state);
    };
    let merge = context.get_op(context.get_value(taken).defining_op()?);
    merge
        .is::<JoinOp>()
        .then(|| merge.dep_operands().get(index).copied())
        .flatten()
}

/// The value a region port or a block argument stands for outside the region.
pub(crate) fn port_incoming(context: &Context, argument: ValueId) -> Option<ValueId> {
    if let Some(region) = context.region_of_port(argument) {
        let handle = context.get_region(region);
        let owner = context.get_op(handle.parent_op()?);
        let deps: Vec<ValueId> = handle.dep_arguments().iter().map(Value::id).collect();
        if let Some(index) = deps.iter().position(|&port| port == argument) {
            return owner.dep_operands().get(index).copied();
        }
        let values: Vec<ValueId> = handle.value_arguments().iter().map(Value::id).collect();
        let index = values.iter().position(|&port| port == argument)?;
        let binding = match owner.clone().as_interface::<dyn Theta>() {
            Some(theta) => theta.carried(),
            None => owner.clone().as_interface::<dyn Gamma>()?.forwarded(),
        };
        return owner
            .value_operands()
            .get(binding.operands.start + index - binding.ports.start)
            .copied();
    }
    let block = context.block_of_argument(argument)?;
    let region = context.parent_region(block)?;
    let op = context.get_op(context.get_region(region).parent_op()?);
    // A gate threads what it was given into each arm, so the arms' arguments are
    // the tail of its operands.
    let arguments = context.get_block(block).arguments().to_vec();
    let index = arguments.iter().position(|a| a.id() == argument)?;
    op.operands()
        .len()
        .checked_sub(arguments.len())
        .and_then(|offset| op.operands().get(offset + index).copied())
}
