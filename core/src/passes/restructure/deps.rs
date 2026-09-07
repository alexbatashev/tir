//! Memory order constructed over ordered blocks, before the order is gone.
//!
//! One chain per object the pointer analysis can name, plus a chain for the
//! memory of unknown provenance. An effect observes its own object's chain and,
//! where it changes memory, every chain it may alias: those are joined into the
//! state it takes and split back out of the state it leaves. Reads fork off a
//! change without ordering one another; the next change, or whatever leaves the
//! block, takes `state.join` of what the fork left, so a read never trails the
//! write that overtakes it.
//!
//! Two accesses whose objects [`Base::distinct`] tells apart share no chain and
//! therefore no edge: independence is a property of the graph, not something a
//! consumer recovers on the side.

use std::collections::{BTreeMap, BTreeSet};

use crate::analysis::alias_facts::{Base, accessed_only, object_base};
use crate::func::CallOp;
use crate::ptr::MemcpyOp;
use crate::state::{JoinOpBuilder, SplitOpBuilder};
use crate::{
    BlockId, Context, MemoryRead, MemoryWrite, OpHandle, OpId, Operation, PassError, RegionId,
    ValueId,
};

use super::cfg::unsupported;

/// The memory one chain stands for: an object the analysis can name, or
/// everything whose provenance it cannot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ChainKey {
    Object(Base),
    World,
}

/// The chains a region's memory is threaded on, and the chains each effect
/// touches — its own first, so a consumer walking an access's chain finds it
/// at index zero of the join it takes and of the split it leaves.
pub struct Plan {
    keys: Vec<ChainKey>,
    touched: BTreeMap<OpId, Vec<usize>>,
}

impl Plan {
    /// How many chains the region is threaded on.
    pub fn chains(&self) -> usize {
        self.keys.len()
    }
}

/// Read the chains `region` needs: one per object its accesses name, plus the
/// world where anything reaches memory the analysis cannot read back.
pub fn plan(context: &Context, region: RegionId) -> Plan {
    let ops = crate::analysis::regions::region_ops(context, region);
    let mut objects = BTreeSet::new();
    let mut world = false;
    for &op in &ops {
        let handle = context.get_op(op);
        if !touches_memory(&handle) {
            continue;
        }
        match accessed_object(&handle).and_then(|address| object_base(context, address)) {
            Some(base) => {
                objects.insert(base);
            }
            None => world = true,
        }
    }
    let keys: Vec<ChainKey> = objects
        .into_iter()
        .map(ChainKey::Object)
        .chain(world.then_some(ChainKey::World))
        .collect();

    let private: Vec<bool> = keys
        .iter()
        .map(|key| match key {
            ChainKey::Object(base) => is_private(context, *base),
            ChainKey::World => false,
        })
        .collect();
    let mut touched = BTreeMap::new();
    for &op in &ops {
        let handle = context.get_op(op);
        if !touches_memory(&handle) {
            continue;
        }
        let base = accessed_object(&handle).and_then(|address| object_base(context, address));
        touched.insert(op, touched_chains(&keys, &private, base));
    }
    for &op in &ops {
        let handle = context.get_op(op);
        if !super::is_ordered_counted_loop(context, &handle) {
            continue;
        }
        let carried: BTreeSet<usize> = crate::analysis::regions::subtree_ops(context, &handle)
            .into_iter()
            .filter_map(|inner| touched.get(&inner))
            .flatten()
            .copied()
            .collect();
        touched.insert(op, carried.into_iter().collect());
    }
    Plan { keys, touched }
}

/// The address an operation accesses, where it names one: a call and a copy of
/// unknown pointers name none, and reach whatever the outside can.
fn accessed_object(op: &OpHandle) -> Option<ValueId> {
    op.clone()
        .as_interface::<dyn MemoryWrite>()
        .map(|write| write.write_location())
        .or_else(|| {
            op.clone()
                .as_interface::<dyn MemoryRead>()
                .map(|read| read.read_location())
        })
}

fn touches_memory(op: &OpHandle) -> bool {
    op.has_interface::<dyn MemoryRead>()
        || op.has_interface::<dyn MemoryWrite>()
        || op.is::<MemcpyOp>()
        || op.is::<CallOp>()
}

/// Whether nothing but the object's own accesses can reach it: a fresh
/// allocation or a parameter the λ declares free of aliases, whose address
/// never leaves those accesses. No pointer of unknown origin and no callee
/// names it, so its chain carries the world's effects on no account.
fn is_private(context: &Context, base: Base) -> bool {
    match base {
        Base::Alloca(pointer)
        | Base::Param {
            pointer,
            noalias: true,
        } => accessed_only(context, pointer),
        _ => false,
    }
}

/// The chains an effect on `base` touches, its own first: every chain whose
/// object it may alias. An effect naming no object reaches the world and every
/// object the world can reach; an effect on a private object reaches nothing
/// else, and nothing else reaches it.
fn touched_chains(keys: &[ChainKey], private: &[bool], base: Option<Base>) -> Vec<usize> {
    let own = keys.iter().position(|key| match (base, key) {
        (Some(base), ChainKey::Object(object)) => base == *object,
        (None, ChainKey::World) => true,
        _ => false,
    });
    let owned_private = own.is_some_and(|own| private[own]);
    let rest = (0..keys.len()).filter(|&index| {
        Some(index) != own
            && match (base, keys[index]) {
                (Some(base), ChainKey::Object(object)) => !base.distinct(object),
                (Some(_), ChainKey::World) => !owned_private,
                (None, _) => !private[index],
            }
    });
    own.into_iter().chain(rest).collect()
}

/// Whether `region` needs a chain constructed: something in it touches memory,
/// and nothing already names a dependency, which would make a second order
/// over the one that is there.
pub fn wants_chain(context: &Context, region: RegionId) -> bool {
    let ops: Vec<OpHandle> = crate::analysis::regions::region_ops(context, region)
        .into_iter()
        .map(|op| context.get_op(op))
        .collect();
    let threaded = ops
        .iter()
        .any(|op| !op.dep_operands().is_empty() || !op.dep_results().is_empty());
    !threaded
        && ops
            .iter()
            .any(|op| !matches!(effect(context, op), Ok(Effect::None)))
}

/// Thread `block`'s operations, its terminator excluded, off the state each
/// chain is entered on, and answer the memory each chain leaves the block with.
/// Joins go before the operation that takes them, splits after the one that
/// leaves them.
pub fn thread_block(
    context: &Context,
    block: BlockId,
    entries: &BTreeMap<usize, ValueId>,
    plan: &Plan,
) -> Result<BTreeMap<usize, ValueId>, PassError> {
    let ops = context.get_block(block).op_ids();
    let (&terminator, body) = ops
        .split_last()
        .ok_or_else(|| unsupported("a block with no terminator"))?;
    let mut chains = Chains {
        context,
        states: entries
            .iter()
            .map(|(&chain, &written)| {
                (
                    chain,
                    ChainState {
                        written,
                        reads: Vec::new(),
                    },
                )
            })
            .collect(),
    };
    for &op in body {
        let handle = context.get_op(op);
        match effect(context, &handle)? {
            Effect::None => {}
            Effect::Read => {
                let own = plan.touched[&op][0];
                let state = chains.state(own)?;
                context.append_dep_operand(op, state.written);
                let left = context.append_dep_result(op);
                chains.state(own)?.reads.push(left);
            }
            Effect::Change => {
                let touched = plan.touched[&op].clone();
                let observed = chains.settle(&touched, op)?;
                context.append_dep_operand(op, observed);
                let published = context.append_dep_result(op);
                chains.split(&touched, op, published)?;
            }
            // A counted loop carries one dependency port per chain its body
            // touches, so it takes one dependency operand per chain rather
            // than the one state a change merges them into.
            Effect::CountedLoop => {
                let touched = plan.touched[&op].clone();
                if touched.is_empty() {
                    continue;
                }
                let mut observed = Vec::with_capacity(touched.len());
                for &chain in &touched {
                    observed.push(chains.close_fork(chain, op)?);
                }
                let body = handle.regions()[0];
                let [body_block] = context.get_region(body).block_ids()[..] else {
                    return Err(unsupported("a counted loop whose body is a graph"));
                };
                let ports: BTreeMap<usize, ValueId> = touched
                    .iter()
                    .map(|&chain| (chain, context.append_dep_block_argument(body_block).id()))
                    .collect();
                let leaving = thread_block(context, body_block, &ports, plan)?;
                let latch = *context.get_block(body_block).op_ids().last().unwrap();
                for &chain in &touched {
                    context.append_dep_operand(latch, leaving[&chain]);
                }
                for state in observed {
                    context.append_dep_operand(op, state);
                }
                for &chain in &touched {
                    let published = context.append_dep_result(op);
                    chains.state(chain)?.written = published;
                }
            }
        }
    }
    let leaving: Vec<usize> = chains.states.keys().copied().collect();
    leaving
        .into_iter()
        .map(|chain| Ok((chain, chains.close_fork(chain, terminator)?)))
        .collect()
}

enum Effect {
    None,
    Read,
    Change,
    /// An `scf.for` the frontend raised: its body is one block, threaded off
    /// the dependency ports the loop carries.
    CountedLoop,
}

fn effect(context: &Context, op: &OpHandle) -> Result<Effect, PassError> {
    if op.has_interface::<dyn MemoryWrite>() || op.is::<MemcpyOp>() || op.is::<CallOp>() {
        return Ok(Effect::Change);
    }
    if op.has_interface::<dyn MemoryRead>() {
        return Ok(Effect::Read);
    }
    if super::is_ordered_counted_loop(context, op) {
        return Ok(Effect::CountedLoop);
    }
    let nested = op
        .regions()
        .iter()
        .flat_map(|&region| crate::analysis::regions::region_ops(context, region))
        .any(|inner| !matches!(effect(context, &context.get_op(inner)), Ok(Effect::None)));
    if nested {
        return Err(unsupported(&format!(
            "memory effects inside {}.{}",
            op.dialect(),
            op.name()
        )));
    }
    Ok(Effect::None)
}

struct ChainState {
    written: ValueId,
    reads: Vec<ValueId>,
}

struct Chains<'a> {
    context: &'a Context,
    states: BTreeMap<usize, ChainState>,
}

impl Chains<'_> {
    fn state(&mut self, chain: usize) -> Result<&mut ChainState, PassError> {
        self.states
            .get_mut(&chain)
            .ok_or_else(|| unsupported("an effect on a chain its region does not carry"))
    }

    /// The memory each of `chains` stands at once the reads forked off it are
    /// closed, joined into one state where the effect crosses several.
    fn settle(&mut self, chains: &[usize], before: OpId) -> Result<ValueId, PassError> {
        let mut settled = Vec::with_capacity(chains.len());
        for &chain in chains {
            settled.push(self.close_fork(chain, before)?);
        }
        Ok(match settled[..] {
            [one] => one,
            _ => self.merge(&settled, before),
        })
    }

    /// The memory after every read of one chain's open fork: the last change
    /// where none forked off it, the one read's state where one did, their
    /// join otherwise.
    fn close_fork(&mut self, chain: usize, before: OpId) -> Result<ValueId, PassError> {
        let state = self.state(chain)?;
        let reads = std::mem::take(&mut state.reads);
        let written = state.written;
        let settled = match reads.len() {
            0 => written,
            1 => reads[0],
            _ => self.merge(&reads, before),
        };
        self.state(chain)?.written = settled;
        Ok(settled)
    }

    /// A `state.join` of `states`, placed where the operation taking it is.
    fn merge(&self, states: &[ValueId], before: OpId) -> ValueId {
        let mut join = JoinOpBuilder::new(self.context).dep_result();
        for &state in states {
            join = join.dep_operand(state);
        }
        let join = join.build();
        self.insert(join.id(), before, 0);
        join.result()
    }

    /// Name each touched chain again after the change that left `published`:
    /// one `state.split` result per chain, in the order the join took them.
    fn split(
        &mut self,
        chains: &[usize],
        after: OpId,
        published: ValueId,
    ) -> Result<(), PassError> {
        if let [one] = chains[..] {
            self.state(one)?.written = published;
            return Ok(());
        }
        let mut split = SplitOpBuilder::new(self.context).dep_operand(published);
        for _ in chains {
            split = split.dep_result();
        }
        let split = split.build();
        self.insert(split.id(), after, 1);
        for (&chain, &state) in chains.iter().zip(&split.states()) {
            self.state(chain)?.written = state;
        }
        Ok(())
    }

    /// Place `op` `offset` operations past `anchor` in the block holding it.
    fn insert(&self, op: OpId, anchor: OpId, offset: usize) {
        let block = self
            .context
            .get_block(self.context.parent_block(anchor).expect("an op in a block"));
        let at = block
            .op_ids()
            .iter()
            .position(|&held| held == anchor)
            .expect("the op sits in its block");
        block.insert(at + offset, op);
    }
}
