use tir_relational::extract::fold;

use crate::{ClassId as Id, Engine, FxHashMap, Label as ENode, RowId};

/// Cheapest representative e-node per e-class, chosen by [`Engine::extract_best`].
///
/// A scope's extraction is a layer over the one it refreshed rather than a copy
/// of it, so it holds only the classes its assumption dirtied and costs the
/// dirty set instead of the graph.
pub struct Extraction<'a, L: ENode> {
    base: Option<&'a Extraction<'a, L>>,
    /// What this layer settled. An explicit `None` is a class the layer
    /// recomputed and found no finite cost for; it must read as nothing rather
    /// than fall through to the base's stale answer.
    best: FxHashMap<Id, Option<Chosen<L>>>,
}

/// What a class was extracted as, and what that spelling cost. The cost is what
/// an incremental re-extraction reads for a class it does not recompute.
#[derive(Clone)]
struct Chosen<L> {
    node: L,
    cost: u64,
}

impl<'a, L: ENode> Extraction<'a, L> {
    /// Chosen node for `id`'s class, or `None` if no node has finite cost. `id` must
    /// be canonical ([`Engine::find`]).
    pub fn node(&self, id: Id) -> Option<&L> {
        self.chosen(id).map(|chosen| &chosen.node)
    }

    /// This extraction with the classes in `dirty`, and only those, recomputed
    /// against `eg` as it stands now.
    ///
    /// A class's answer depends on its own rows and its children's costs, so
    /// `dirty` must name every class changed since `self` was taken, closed
    /// upward over parents: [`Engine::scope_dirty`] where `self` is the base
    /// extraction, [`Engine::innermost_dirty`] where it is the one the enclosing
    /// scope refreshed. A class outside it has the rows and
    /// the child costs it had when `self` was taken, so it keeps its answer.
    /// Ascending order makes the sweep the same scan a full pass takes over those
    /// classes, which is what keeps the cost ties broken the same way.
    pub fn refresh<'b>(
        &'b self,
        eg: &Engine<L>,
        dirty: &[Id],
        cost_of: impl Fn(Id, &L) -> u64,
    ) -> Extraction<'b, L> {
        let dirty: Vec<u32> = dirty.iter().map(|id| id.0).collect();
        eg.extraction(&dirty, Some(self), cost_of)
    }

    fn cost(&self, id: Id) -> Option<u64> {
        self.chosen(id).map(|chosen| chosen.cost)
    }

    /// What the innermost layer holding `id` settled on, `None` once a layer
    /// says the class has no finite cost.
    fn chosen(&self, id: Id) -> Option<&Chosen<L>> {
        match self.best.get(&id) {
            Some(settled) => settled.as_ref(),
            None => self.base?.chosen(id),
        }
    }
}

impl<L: ENode> Engine<L> {
    /// Greedy bottom-up extraction: per class, the node minimizing
    /// `cost_of(class, node)` plus each child's chosen cost. The class is the
    /// canonical id the node would represent, so a cost model may reject a form
    /// the class cannot be spelled in. Cycle-tolerant — a node with un-costed
    /// children is skipped and revisited to a fixpoint, so a cycle is costed
    /// through its non-cyclic input. Scope-aware via
    /// [`Engine::classes`]/[`Engine::find`].
    pub fn extract_best(&self, cost_of: impl Fn(Id, &L) -> u64) -> Extraction<'static, L> {
        let classes: Vec<u32> = self.class_ids().map(|id| id.0).collect();
        self.extraction(&classes, None, cost_of)
    }

    /// Fold `classes` over their e-nodes in class-list order, a child outside
    /// them costed from `base`, and record the winners as a layer over `base`.
    fn extraction<'b>(
        &self,
        classes: &[u32],
        base: Option<&'b Extraction<'b, L>>,
        cost_of: impl Fn(Id, &L) -> u64,
    ) -> Extraction<'b, L> {
        let outside = |child| base.and_then(|base| base.cost(Id(child)));
        let chosen = fold(self.class_count(), classes, &outside, |class, out| {
            for row in self.rows(Id(class)) {
                let node = self.node(row);
                let children = node.children().iter().map(|&child| self.find(child).0);
                out.push(row.0, cost_of(Id(class), node), children);
            }
        });
        let mut best = FxHashMap::default();
        for (&class, chosen) in classes.iter().zip(chosen) {
            match chosen {
                Some((row, cost)) => {
                    let node = self.node(RowId(row)).clone();
                    best.insert(Id(class), Some(Chosen { node, cost }));
                }
                // Only a layer has a base to shadow; the bottom one says nothing
                // about a class it could not cost.
                None if base.is_some() => {
                    best.insert(Id(class), None);
                }
                None => {}
            }
        }
        Extraction { base, best }
    }
}
