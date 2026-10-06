//! Extraction as a fold: the least cost of each id, where an id costs the least
//! of its members and a member costs its own price plus its children's.

use crate::Csr;

/// No slot: an id the fold does not cover.
const NONE: u32 = u32::MAX;

/// Where a client hands over the members of one id.
pub struct Members<'a> {
    index: &'a [u32],
    outside: &'a dyn Fn(u32) -> Option<u64>,
    table: &'a mut Table,
    edges: &'a mut Vec<(u32, u32)>,
    slot: usize,
}

impl Members<'_> {
    /// One member of the id: its tag, its own price and its children.
    pub fn push(&mut self, tag: u32, price: u64, children: impl IntoIterator<Item = u32>) {
        let position = self.table.entries.len() as u32;
        let start = self.table.children.len();
        let mut member = Entry {
            tag,
            slot: self.slot,
            base: price,
            children: 0..0,
            costable: true,
        };
        for child in children {
            match self.index[child as usize] {
                NONE => match (self.outside)(child) {
                    Some(cost) => member.base = member.base.saturating_add(cost),
                    None => member.costable = false,
                },
                child => {
                    self.table.children.push(child as usize);
                    self.edges.push((child, position));
                }
            }
        }
        member.children = start..self.table.children.len();
        self.table.entries.push(member);
    }
}

/// Least cost per id of `ids`, and the tag of the member that spells it.
///
/// `members(id, out)` pushes every member of `id` onto `out`. A member costs
/// its price plus each child's least cost; a child outside `ids` is priced by
/// `outside`, and a member with a child nothing prices is never chosen.
/// Members wait for un-costed children and the fold runs to a fixpoint, so a
/// cycle is costed through its other inputs. On a tie the first member in scan
/// order wins: `ids` in order, then each id's members in the order pushed.
/// `bound` is one past the largest id a child names.
pub fn fold(
    bound: usize,
    ids: &[u32],
    outside: &dyn Fn(u32) -> Option<u64>,
    mut members: impl FnMut(u32, &mut Members<'_>),
) -> Vec<Option<(u32, u64)>> {
    let started = crate::telemetry::enabled().then(std::time::Instant::now);
    // Id -> slot, as a dense array: ids run to the high-water mark, absorbed
    // ones included, but the array is a fraction of what a probe per child
    // costs.
    let mut index: Vec<u32> = vec![NONE; bound];
    for (slot, &id) in ids.iter().enumerate() {
        index[id as usize] = slot as u32;
    }
    let mut table = Table {
        entries: Vec::new(),
        parents: Csr::default(),
        children: Vec::new(),
    };
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for (slot, &id) in ids.iter().enumerate() {
        let mut out = Members {
            index: &index,
            outside,
            table: &mut table,
            edges: &mut edges,
            slot,
        };
        members(id, &mut out);
    }
    table.parents = Csr::build(ids.len(), edges);
    let cost = table.costs(ids.len());

    // The winner is the first member in scan order that spells its id at the
    // settled cost. Reading it off the costs rather than off the fixpoint's
    // improvement order makes a tie break the same way whatever schedule
    // reached those costs, which an incremental fold over part of the ids,
    // starting from costs an earlier fold settled, needs.
    let mut chosen: Vec<Option<(u32, u64)>> = vec![None; ids.len()];
    for member in &table.entries {
        if let Some(settled) = cost[member.slot]
            && chosen[member.slot].is_none()
            && table.cost_of(member, &cost) == Some(settled)
        {
            chosen[member.slot] = Some((member.tag, settled));
        }
    }
    if let Some(started) = started {
        crate::telemetry::count_extract(started.elapsed());
    }
    chosen
}

/// The ids flattened for the fold: dense slots and members in scan order, with
/// prices and child slots resolved once so the fixpoint touches neither the
/// union-find nor a hash map.
struct Table {
    entries: Vec<Entry>,
    /// Slot -> positions of the members taking it as a child, ascending.
    parents: Csr,
    /// Child slots, sliced by [`Entry::children`].
    children: Vec<usize>,
}

struct Entry {
    tag: u32,
    slot: usize,
    /// Price, plus the cost of every child outside the fold.
    base: u64,
    children: std::ops::Range<usize>,
    /// A child outside the fold that nothing prices can never be costed, so
    /// neither can this member.
    costable: bool,
}

impl Table {
    /// Least cost per slot, or `None` where no member has a finite one.
    ///
    /// Only the parents of a slot improved in the previous sweep can improve in
    /// the next, so `dirty` shrinks to the propagation frontier.
    fn costs(&self, slots: usize) -> Vec<Option<u64>> {
        let mut cost: Vec<Option<u64>> = vec![None; slots];
        let mut dirty = vec![true; self.entries.len()];
        let mut next = vec![false; self.entries.len()];
        let mut queued = self.entries.len();
        while queued > 0 {
            queued = 0;
            for position in 0..self.entries.len() {
                if !std::mem::replace(&mut dirty[position], false) {
                    continue;
                }
                let member = &self.entries[position];
                let Some(total) = self.cost_of(member, &cost) else {
                    continue;
                };
                if cost[member.slot].is_none_or(|best| total < best) {
                    cost[member.slot] = Some(total);
                    for &parent in self.parents.get(member.slot as u32) {
                        // A parent still ahead of the sweep sees the improvement
                        // in this round; an earlier one waits for the next.
                        let parent = parent as usize;
                        if parent > position {
                            dirty[parent] = true;
                        } else if !std::mem::replace(&mut next[parent], true) {
                            queued += 1;
                        }
                    }
                }
            }
            std::mem::swap(&mut dirty, &mut next);
            next.fill(false);
        }
        cost
    }

    /// The member's base plus each child slot's best cost, or `None` if any
    /// child is un-costed.
    fn cost_of(&self, member: &Entry, cost: &[Option<u64>]) -> Option<u64> {
        if !member.costable {
            return None;
        }
        let mut total = member.base;
        for &slot in &self.children[member.children.clone()] {
            total = total.saturating_add(cost[slot]?);
        }
        Some(total)
    }
}
