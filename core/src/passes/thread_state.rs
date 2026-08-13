//! Memory state threading.
//!
//! Memory order becomes an explicit def-use edge: the function's memory at entry
//! is named by `builtin.entry_state`, and every operation that touches memory
//! consumes the state it observes and produces the state it leaves behind. A slot
//! whose address never escapes is a memory of its own, so it gets a chain rooted
//! at its allocation; everything else — escaping slots, unknown pointers, calls —
//! shares one conservative chain, which the function's `return` exports.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::analysis::AnalysisManager;
use crate::analysis::slots::collect_slots;
use crate::builtin::{CallOp, EntryStateOpBuilder, FuncOp, IndirectCallOp, ReturnOp, StateType};
use crate::ptr::MemcpyOp;
use crate::{
    Block, Context, MemoryRead, MemoryWrite, OpId, OpInstance, Operation, OperationRef, Pass,
    PassError, PassTarget, PromotableAllocation, RegionId, Rewriter, TypeId, ValueId, scf,
};

#[derive(Default)]
pub struct ThreadStatePass;

impl ThreadStatePass {
    pub fn new() -> Self {
        Self
    }
}

crate::register_pass!(ThreadStatePass, "thread-state");

impl Pass for ThreadStatePass {
    fn name(&self) -> &'static str {
        "thread-state"
    }

    fn target(&self) -> PassTarget {
        PassTarget::operation::<FuncOp>()
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        _rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<(), PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(());
        }
        let Some(&body) = op.op().regions.first() else {
            return Ok(());
        };
        let block_ids = context.get_region(body).block_ids();
        let [entry] = block_ids[..] else {
            return Ok(());
        };

        let ops = region_ops(context, body);
        if !ops
            .iter()
            .any(|&op_id| touches_memory(&context.get_op(op_id)))
        {
            return Ok(());
        }

        let entry_block = context.get_block(entry);
        if !threadable(context, &entry_block) {
            return Ok(());
        }

        let tracked = collect_slots(context, &ops)
            .into_iter()
            .filter(|(_, slot)| slot.alloca.is_some() && !slot.escapes)
            .map(|(pointer, _)| pointer)
            .collect();

        let state = StateType::new(context);
        let root = EntryStateOpBuilder::new(context).result_type(state).build();
        entry_block.insert(0, root.id());

        Threader {
            context,
            state,
            tracked,
            chains: BTreeMap::from([(Chain::Conservative, root.result())]),
        }
        .walk(&entry_block);

        Ok(())
    }
}

/// The memory a chain of state values describes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Chain {
    /// Everything that may alias: escaping slots, unknown pointers, and whatever a
    /// call touches.
    Conservative,
    /// One slot whose address never leaves the function, named by its allocation.
    Slot(ValueId),
}

/// How one operation relates to the state chains.
enum Effect {
    /// Opens a chain of its own.
    Open(ValueId),
    /// Observes a chain and leaves a new state behind.
    Access(Chain),
    /// Hands the conservative chain to the function's caller.
    Export,
}

struct Threader<'a> {
    context: &'a Context,
    state: TypeId,
    /// The slots that get a chain of their own.
    tracked: BTreeSet<ValueId>,
    /// The state each chain has reached at the point being walked.
    chains: BTreeMap<Chain, ValueId>,
}

impl Threader<'_> {
    fn walk(&mut self, block: &Arc<Block>) {
        for op_id in block.op_ids() {
            let op = self.context.get_op(op_id);
            match classify(&op, &self.tracked) {
                Some(Effect::Open(slot)) if self.tracked.contains(&slot) => {
                    let result = self.context.grow_port(op_id, self.state, None, |_, _| None);
                    self.chains.insert(Chain::Slot(slot), result);
                }
                Some(Effect::Access(chain)) => {
                    let observed = self.chain(chain);
                    let result =
                        self.context
                            .grow_port(op_id, self.state, Some(observed), |_, _| None);
                    self.chains.insert(chain, result);
                }
                Some(Effect::Export) => {
                    let exported = self.chain(Chain::Conservative);
                    self.context.append_operand(op_id, exported);
                }
                // An escaping slot is observable from anywhere, so it opens no
                // chain of its own: its accesses join the conservative one.
                Some(Effect::Open(_)) => {}
                None if subtree_touches_memory(self.context, &op) => self.thread_regions(&op),
                None => {}
            }
        }
    }

    /// Carry every chain the op's regions touch across it as a port of its own:
    /// the state entering the op is the incoming operand, each region threads the
    /// argument it receives and carries the state it leaves out through its
    /// terminator, and the op's result is the state that reaches the code after.
    ///
    /// A loop carries the port across its iterations and a γ across its arms, but
    /// the edits are the same: what differs is only that a γ's arms are
    /// alternatives, so each threads its own copy of the state entering the gate
    /// and the result is whichever copy the arm that ran left behind.
    fn thread_regions(&mut self, op: &Arc<OpInstance>) {
        let context = self.context;
        let carried = self
            .chains
            .keys()
            .copied()
            .filter(|&chain| self.touches_chain(op, chain))
            .collect::<Vec<_>>();

        let entries = op
            .regions
            .iter()
            .map(|&region| context.get_block(context.get_region(region).block_ids()[0]))
            .collect::<Vec<_>>();

        let arguments = entries
            .iter()
            .map(|entry| {
                carried
                    .iter()
                    .map(|_| context.append_block_argument(entry.id(), self.state).id())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        for (entry, arguments) in entries.iter().zip(&arguments) {
            let outer = std::mem::replace(
                &mut self.chains,
                carried
                    .iter()
                    .copied()
                    .zip(arguments.iter().copied())
                    .collect(),
            );
            self.walk(entry);
            let leaving = carried
                .iter()
                .map(|&chain| self.chain(chain))
                .collect::<Vec<_>>();
            self.chains = outer;

            let terminator = *entry.op_ids().last().expect("a region is terminated");
            for value in leaving {
                context.append_operand(terminator, value);
            }
        }

        for &chain in &carried {
            let init = self.chain(chain);
            context.append_operand(op.id, init);
        }
        for &chain in &carried {
            // The ports the op already carries are wired up; all that is left is
            // the result naming the state it leaves behind.
            let result = context.grow_port(op.id, self.state, None, |_, _| None);
            self.chains.insert(chain, result);
        }
    }

    /// Whether anything inside `op`'s regions accesses `chain`.
    fn touches_chain(&self, op: &Arc<OpInstance>, chain: Chain) -> bool {
        op.regions
            .iter()
            .flat_map(|&region| region_ops(self.context, region))
            .any(|op_id| {
                matches!(
                    classify(&self.context.get_op(op_id), &self.tracked),
                    Some(Effect::Access(accessed)) if accessed == chain
                )
            })
    }

    fn chain(&self, chain: Chain) -> ValueId {
        *self
            .chains
            .get(&chain)
            .expect("the chain reaches this point")
    }
}

/// How `op` relates to the chains, reading an access through an untracked pointer
/// as one that may alias anything.
fn classify(op: &Arc<OpInstance>, tracked: &BTreeSet<ValueId>) -> Option<Effect> {
    let chain_of = |pointer| {
        if tracked.contains(&pointer) {
            Chain::Slot(pointer)
        } else {
            Chain::Conservative
        }
    };
    if let Some(allocation) = op.clone().as_interface::<dyn PromotableAllocation>() {
        return Some(Effect::Open(allocation.promoted_location()));
    }
    if let Some(read) = op.clone().as_interface::<dyn MemoryRead>() {
        return Some(Effect::Access(chain_of(read.read_location())));
    }
    if let Some(write) = op.clone().as_interface::<dyn MemoryWrite>() {
        return Some(Effect::Access(chain_of(write.write_location())));
    }
    if op.is::<MemcpyOp>() || op.is::<CallOp>() || op.is::<IndirectCallOp>() {
        return Some(Effect::Access(Chain::Conservative));
    }
    op.is::<ReturnOp>().then_some(Effect::Export)
}

/// Whether `op` is one of the operations that carry state. Exporting the chain is
/// not touching memory: a `return` alone leaves a function with nothing to thread.
fn touches_memory(op: &Arc<OpInstance>) -> bool {
    matches!(
        classify(op, &BTreeSet::new()),
        Some(Effect::Open(_) | Effect::Access(_))
    )
}

/// Whether every operation touching memory sits where a chain can reach it: a
/// chain crosses a region boundary as a port, which `scf`'s loops and gates have.
/// An op whose regions touch no memory is transparent and needs nothing.
///
/// A loop with an early exit is not threadable: its `break`/`continue` would have
/// to carry the chain out along an edge of its own.
fn threadable(context: &Context, block: &Arc<Block>) -> bool {
    block.op_ids().iter().all(|&op_id| {
        let op = context.get_op(op_id);
        if op.regions.is_empty() || !subtree_touches_memory(context, &op) {
            return true;
        }
        if !carries_state(&op) {
            return false;
        }
        op.regions.iter().all(|&region| {
            let blocks = context.get_region(region).block_ids();
            let [entry] = blocks[..] else {
                return false;
            };
            !region_ops(context, region)
                .iter()
                .any(|&nested| exits_early(&context.get_op(nested)))
                && threadable(context, &context.get_block(entry))
        })
    })
}

/// Whether `op` can carry a chain across its regions as a port.
fn carries_state(op: &Arc<OpInstance>) -> bool {
    op.is::<scf::ForOp>()
        || op.is::<scf::WhileOp>()
        || op.is::<scf::IfOp>()
        || op.is::<scf::SwitchOp>()
}

fn exits_early(op: &Arc<OpInstance>) -> bool {
    op.is::<scf::BreakOp>() || op.is::<scf::ContinueOp>()
}

fn subtree_touches_memory(context: &Context, op: &Arc<OpInstance>) -> bool {
    op.regions
        .iter()
        .flat_map(|&region| region_ops(context, region))
        .any(|op_id| touches_memory(&context.get_op(op_id)))
}

fn block_ops(context: &Context, block: &Arc<Block>) -> Vec<OpId> {
    let mut ops = Vec::new();
    for op_id in block.op_ids() {
        ops.push(op_id);
        for &region in &context.get_op(op_id).regions {
            ops.extend(region_ops(context, region));
        }
    }
    ops
}

/// Every operation in a region tree, outermost first.
fn region_ops(context: &Context, region: RegionId) -> Vec<OpId> {
    context
        .get_region(region)
        .iter(context.clone())
        .flat_map(|block| block_ops(context, &block))
        .collect()
}
