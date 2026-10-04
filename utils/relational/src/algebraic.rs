//! Query-directed matching of binary algebraic operators.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::{Atom, ClassId, Engine, Label, LabelId, RowId, Scalar, Var};

/// Laws a rule requests. Concrete labels must authorize each requested law.
#[derive(Clone, Copy, Debug, Default)]
pub struct AlgebraicOptions {
    pub associative: bool,
    pub commutative: bool,
}

/// An expression deferred until a successful match's head uses its variable.
/// Its finite proof shares an immutable arena and names existing classes.
#[derive(Clone, Debug)]
pub struct Residual {
    pub var: Var,
    pub label: LabelId,
    pub(crate) expr: ResidualExpr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProofRef {
    Class(ClassId),
    Join(usize),
}

type ProofArena = Arc<[(ProofRef, ProofRef)]>;

#[derive(Clone, Copy, Debug)]
enum ProofRoot {
    Ref(ProofRef),
    // A row-sensitive match combines two frozen table states without copying
    // their arena or losing the physical row that proved the match.
    Join(ProofRef, ProofRef),
}

#[derive(Clone, Debug)]
pub(crate) struct ResidualExpr {
    arena: ProofArena,
    root: ProofRoot,
}

#[derive(Default)]
struct ProofBuilder {
    nodes: Vec<(ProofRef, ProofRef)>,
    joins: BTreeMap<(ProofRef, ProofRef), usize>,
}

impl ProofBuilder {
    fn join(&mut self, a: ProofRef, b: ProofRef) -> ProofRef {
        let pair = (a, b);
        let next = self.nodes.len();
        let id = *self.joins.entry(pair).or_insert_with(|| {
            self.nodes.push(pair);
            next
        });
        ProofRef::Join(id)
    }

    fn combine(&mut self, a: Option<ProofRef>, b: Option<ProofRef>) -> Option<ProofRef> {
        match (a, b) {
            (Some(a), Some(b)) => Some(self.join(a, b)),
            (a, b) => a.or(b),
        }
    }

    fn bag(&mut self, counts: &BTreeMap<ClassId, usize>) -> Option<ProofRef> {
        let mut result = None;
        for (&class, &count) in counts {
            let mut count = count;
            let mut power = ProofRef::Class(class);
            while count != 0 {
                if count & 1 != 0 {
                    result = self.combine(result, Some(power));
                }
                count >>= 1;
                if count != 0 {
                    power = self.join(power, power);
                }
            }
        }
        result
    }

    fn freeze(self) -> ProofArena {
        self.nodes.into()
    }
}

impl Residual {
    pub(crate) fn materialize<L: Label>(&self, eg: &mut Engine<L>) -> ClassId {
        let template = eg
            .label_node(self.label)
            .expect("matched label exists")
            .clone();
        let mut memo = BTreeMap::new();
        let mut pending = match self.expr.root {
            ProofRoot::Ref(root) => vec![root],
            ProofRoot::Join(a, b) => vec![a, b],
        };
        while let Some(reference) = pending.pop() {
            let ProofRef::Join(id) = reference else {
                continue;
            };
            if memo.contains_key(&id) {
                continue;
            }
            let (a, b) = self.expr.arena[id];
            let unresolved = [a, b]
                .into_iter()
                .filter(|child| matches!(child, ProofRef::Join(id) if !memo.contains_key(id)))
                .collect::<SmallVec<[_; 2]>>();
            if !unresolved.is_empty() {
                pending.push(reference);
                pending.extend(unresolved);
                continue;
            }
            let class = insert_pair(
                eg,
                self.label,
                &template,
                resolve(eg, a, &memo),
                resolve(eg, b, &memo),
            );
            memo.insert(id, class);
        }
        match self.expr.root {
            ProofRoot::Ref(root) => resolve(eg, root, &memo),
            ProofRoot::Join(a, b) => insert_pair(
                eg,
                self.label,
                &template,
                resolve(eg, a, &memo),
                resolve(eg, b, &memo),
            ),
        }
    }
}

fn resolve<L: Label>(
    eg: &Engine<L>,
    reference: ProofRef,
    memo: &BTreeMap<usize, ClassId>,
) -> ClassId {
    eg.find(match reference {
        ProofRef::Class(class) => class,
        ProofRef::Join(id) => memo[&id],
    })
}

fn insert_pair<L: Label>(
    eg: &mut Engine<L>,
    label: LabelId,
    template: &L,
    a: ClassId,
    b: ClassId,
) -> ClassId {
    let mut children = [eg.find(a), eg.find(b)];
    if template.commutative() {
        children.sort_unstable();
    }
    eg.add_labelled(label, &[label.0, children[0].0, children[1].0], || {
        template.reassociate(&children)
    })
}

/// Replace the root's binary node atoms with one algebraic atom. Compatible
/// nested atoms flatten only for associativity; other operators remain selectors.
/// `remainder` must occur once and have no constraints outside the derived type.
pub fn lower_algebraic<L: Label>(
    atoms: &mut Vec<Atom<L>>,
    root: Var,
    remainder: Option<Var>,
    options: AlgebraicOptions,
    remainder_type: Option<Scalar>,
) -> Result<(), String> {
    let index = atoms
        .iter()
        .position(|atom| matches!(atom, Atom::Node { class, .. } if *class == root))
        .ok_or("algebraic matching requires an operation at the rule root")?;
    let Atom::Node {
        template,
        args,
        row,
        ..
    } = &atoms[index]
    else {
        unreachable!()
    };
    if args.len() != 2 {
        return Err("algebraic matching requires a binary root operation".into());
    }
    let template = template.clone();
    let row = *row;
    if !options.associative && !options.commutative {
        return Err("algebraic matching requires an associative or commutative option".into());
    }
    let mut pending: Vec<Var> = args.iter().rev().copied().collect();
    let mut consumed = vec![index];
    let mut selectors = SmallVec::new();
    while let Some(var) = pending.pop() {
        let nested = options.associative.then(|| atoms.iter().enumerate().find(|(i, atom)| {
            !consumed.contains(i) && matches!(atom, Atom::Node { template: other, class, args, .. }
                if *class == var && args.len() == 2 && template.matches(other) && other.matches(&template))
        })).flatten();
        if let Some((index, Atom::Node { args, row, .. })) = nested {
            if row.is_some() {
                return Err("a flattened operation cannot expose a row to guards".into());
            }
            consumed.push(index);
            pending.extend(args.iter().rev().copied());
        } else {
            selectors.push(var);
        }
    }
    if let Some(var) = remainder {
        if selectors
            .iter()
            .filter(|&&selector| selector == var)
            .count()
            != 1
        {
            return Err("an algebraic remainder must occur exactly once".into());
        }
        selectors.retain(|selector| *selector != var);
        if atoms
            .iter()
            .enumerate()
            .any(|(i, atom)| !consumed.contains(&i) && atom.class() == var)
        {
            return Err("an algebraic remainder cannot have structural or fact constraints".into());
        }
    } else if remainder_type.is_some() {
        return Err("a remainder type needs a remainder variable".into());
    }
    for &removed in consumed.iter().filter(|&&removed| removed != index) {
        let var = atoms[removed].class();
        if atoms.iter().enumerate().any(|(i, atom)| {
            !consumed.contains(&i)
                && (atom.class() == var
                    || match atom {
                        Atom::Node { args, .. } => args.contains(&var),
                        Atom::Algebraic { selectors, .. } => selectors.contains(&var),
                        Atom::Object { base, .. } => *base == var,
                        _ => false,
                    })
        }) {
            return Err("a flattened operation cannot carry independent class constraints".into());
        }
    }
    atoms[index] = Atom::Algebraic {
        template,
        selectors,
        remainder,
        class: root,
        row,
        associative: options.associative,
        commutative: options.commutative,
        remainder_type,
    };
    let mut position = 0;
    atoms.retain(|_| {
        let keep = position == index || !consumed.contains(&position);
        position += 1;
        keep
    });
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct Witness {
    pub bindings: Vec<Option<ClassId>>,
    pub residual: Option<ResidualExpr>,
    pub(crate) cost: usize,
}

#[derive(Clone, Debug)]
struct StateWitness {
    bindings: Vec<Option<ClassId>>,
    residual: Option<ProofRef>,
    cost: usize,
}

impl StateWitness {
    fn owned(self, arena: &ProofArena) -> Witness {
        Witness {
            bindings: self.bindings,
            residual: self.residual.map(|root| ResidualExpr {
                arena: arena.clone(),
                root: ProofRoot::Ref(root),
            }),
            cost: self.cost,
        }
    }
}

fn ordered(bindings: &[Option<ClassId>], orders: &[(usize, usize)]) -> bool {
    orders.iter().all(|&(a, b)| {
        match (
            bindings.get(a).copied().flatten(),
            bindings.get(b).copied().flatten(),
        ) {
            (Some(a), Some(b)) => a <= b,
            _ => true,
        }
    })
}

type Key = (Vec<Option<ClassId>>, bool);

struct State {
    witness: StateWitness,
    queued: bool,
}

#[derive(Default)]
struct States {
    entries: BTreeMap<Key, usize>,
    records: Vec<State>,
    by_var: BTreeMap<(Var, Option<ClassId>), Vec<usize>>,
    by_subset: BTreeMap<Vec<bool>, Vec<usize>>,
}

impl States {
    fn admits(&self, bindings: &[Option<ClassId>], residual: bool, cost: usize) -> bool {
        self.entries
            .get(&(bindings.to_vec(), residual))
            .is_none_or(|&id| self.records[id].witness.cost > cost)
    }

    fn retain(&mut self, witness: StateWitness, repeated: &[(Var, Vec<usize>)]) -> Option<usize> {
        let key = (witness.bindings.clone(), witness.residual.is_some());
        match self.entries.entry(key) {
            std::collections::btree_map::Entry::Occupied(entry) => {
                let id = *entry.get();
                let prior = &mut self.records[id].witness;
                if prior.cost <= witness.cost {
                    return None;
                }
                *prior = witness;
                Some(id)
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                let id = self.records.len();
                self.by_subset
                    .entry(witness.bindings.iter().map(Option::is_some).collect())
                    .or_default()
                    .push(id);
                for (var, slots) in repeated {
                    let value = slots.iter().find_map(|&slot| witness.bindings[slot]);
                    self.by_var.entry((*var, value)).or_default().push(id);
                }
                entry.insert(id);
                self.records.push(State {
                    witness,
                    queued: false,
                });
                Some(id)
            }
        }
    }

    fn compatible(
        &self,
        witness: &StateWitness,
        repeated: &[(Var, Vec<usize>)],
        complete: bool,
    ) -> Vec<usize> {
        let matches_subset = |&id: &usize| {
            witness
                .bindings
                .iter()
                .zip(&self.records[id].witness.bindings)
                .all(|(a, b)| {
                    if complete {
                        a.is_some() != b.is_some()
                    } else {
                        a.is_none() || b.is_none()
                    }
                })
        };
        if let Some((var, value)) = repeated.iter().find_map(|(var, slots)| {
            slots
                .iter()
                .find_map(|&slot| witness.bindings[slot])
                .map(|value| (*var, value))
        }) {
            [None, Some(value)]
                .into_iter()
                .flat_map(|value| self.by_var.get(&(var, value)).into_iter().flatten())
                .copied()
                .filter(matches_subset)
                .collect()
        } else {
            self.by_subset
                .iter()
                .filter(|(subset, _)| {
                    witness
                        .bindings
                        .iter()
                        .zip(subset.iter())
                        .all(|(bound, &present)| {
                            if complete {
                                bound.is_some() != present
                            } else {
                                bound.is_none() || !present
                            }
                        })
                })
                .flat_map(|(_, ids)| ids.iter())
                .copied()
                .collect()
        }
    }
}

fn combined_bindings(
    a: &StateWitness,
    b: &StateWitness,
    repeated: &[(Var, Vec<usize>)],
) -> Option<Vec<Option<ClassId>>> {
    if a.bindings.iter().all(Option::is_none) && b.bindings.iter().all(Option::is_none) {
        return None;
    }
    if a.bindings
        .iter()
        .zip(&b.bindings)
        .any(|(a, b)| a.is_some() && b.is_some())
    {
        return None;
    }
    for (_, slots) in repeated {
        let mut known = None;
        for &slot in slots {
            if let Some(value) = a.bindings[slot].or(b.bindings[slot]) {
                if known.is_some_and(|prior| prior != value) {
                    return None;
                }
                known = Some(value);
            }
        }
    }
    Some(
        a.bindings
            .iter()
            .zip(&b.bindings)
            .map(|(a, b)| a.or(*b))
            .collect(),
    )
}

fn combined_cost(a: &StateWitness, b: &StateWitness) -> usize {
    a.cost.saturating_add(b.cost).saturating_add(1)
}

/// Shared only for one frozen search. Each concrete label has its own states,
/// so root templates cannot erase type or attribute boundaries.
#[derive(Default)]
pub(crate) struct AlgebraicCache {
    tables: BTreeMap<LabelId, AlgebraicSearch>,
    ground: BTreeMap<LabelId, Option<GroundSearch>>,
}

struct GroundSearch {
    candidates: Vec<(ClassId, Vec<(ClassId, usize)>)>,
    anchors: BTreeMap<ClassId, Vec<usize>>,
    rows: BTreeMap<ClassId, RowId>,
    unresolved: Vec<ClassId>,
}

impl GroundSearch {
    fn new<L: Label>(eg: &Engine<L>, label: LabelId) -> Option<Self> {
        use crate::identity::GroundKey;
        let identity = eg.algebraic_identity();
        let laws = identity.policies.get(&label)?;
        if identity.dirty.contains(&label) || !laws.associative || !laws.commutative {
            return None;
        }
        let mut candidates = BTreeSet::new();
        let mut rows = BTreeMap::new();
        let mut unresolved = Vec::new();
        if let Some(table) = eg.table(label) {
            for (offset, &found) in table.column(0).iter().enumerate() {
                if found == label.0 {
                    let class = eg.find(ClassId(table.column(table.arity())[offset]));
                    rows.entry(class).or_insert(RowId(table.tags()[offset]));
                }
            }
        }
        // Preserve the class's witness order after discovering only family owners.
        for (&class, row) in &mut rows {
            *row = eg.rows(class).find(|&row| eg.label(row) == label).unwrap();
        }
        unresolved.extend(
            rows.keys()
                .copied()
                .filter(|&class| !identity.certified.contains_key(&(label, class))),
        );
        let mut atoms = BTreeSet::new();
        for ((family, key), &class) in &identity.proofs {
            if *family == label
                && let GroundKey::Bag(key) = key
            {
                atoms.extend(key.iter().map(|&(atom, _)| atom));
                candidates.insert((eg.find(class), key.clone()));
            }
        }
        for ((family, _), key) in &identity.certified {
            if *family == label
                && let GroundKey::Bag(key) = key
            {
                atoms.extend(key.iter().map(|&(atom, _)| atom));
            }
        }
        for atom in atoms {
            if !rows.contains_key(&atom) {
                candidates.insert((atom, vec![(atom, 1)]));
            }
        }
        let candidates: Vec<_> = candidates.into_iter().collect();
        let mut anchors = BTreeMap::<_, Vec<_>>::new();
        for (index, (_, key)) in candidates.iter().enumerate() {
            if let Some(&(atom, _)) = key.first() {
                anchors.entry(atom).or_default().push(index);
            }
        }
        Some(Self {
            candidates,
            anchors,
            rows,
            unresolved,
        })
    }
}

struct Relation {
    classes: Vec<ClassId>,
    slots: Vec<usize>,
    rows: Vec<Vec<(RowId, usize, usize)>>,
    parents: Vec<Vec<(usize, usize)>>,
}

impl Relation {
    fn new<L: Label>(eg: &Engine<L>, label: LabelId) -> Self {
        let mut classes = Vec::new();
        let mut found_rows = Vec::new();
        let table = eg.table(label).expect("a matched label has rows");
        for (offset, &found) in table.column(0).iter().enumerate() {
            if found != label.0 {
                continue;
            }
            let class = eg.find(ClassId(table.column(table.arity())[offset]));
            let row = RowId(table.tags()[offset]);
            let children = eg.children(row);
            let (left, right) = (children[0], children[1]);
            classes.extend([class, left, right]);
            found_rows.push((class, row, left, right));
        }
        classes.sort_unstable();
        classes.dedup();
        // Sorted local slots preserve class visitation while keeping scoped ID
        // capacity out of the row, parent, count, and state payloads.
        let mut slots = vec![usize::MAX; eg.class_count()];
        for (slot, class) in classes.iter().enumerate() {
            slots[class.index()] = slot;
        }
        let mut rows = vec![Vec::new(); classes.len()];
        let mut parents = vec![Vec::new(); classes.len()];
        for (class, row, left, right) in found_rows {
            let (class, left, right) = (
                slots[class.index()],
                slots[left.index()],
                slots[right.index()],
            );
            rows[class].push((row, left, right));
            parents[left].push((class, right));
            parents[right].push((class, left));
        }
        for edges in &mut parents {
            edges.sort_unstable();
            edges.dedup();
        }
        Self {
            classes,
            slots,
            rows,
            parents,
        }
    }

    /// Counts accepted occurrences without committing their selector bindings.
    /// Alternatives take a maximum and duplicate operands count separately.
    /// Capping at the requested number preserves the admission decision. The
    /// least fixed point admits finite cyclic witnesses without inventing leaves.
    fn availability(
        &self,
        selectors: &[Var],
        accepts: &dyn Fn(Var, ClassId) -> bool,
    ) -> Vec<usize> {
        let required = selectors.len();
        let mut counts = vec![0; self.classes.len()];
        let mut queue = std::collections::VecDeque::new();
        let mut pending = vec![false; self.classes.len()];
        for (slot, &class) in self.classes.iter().enumerate() {
            let count = usize::from(selectors.iter().any(|&var| accepts(var, class)));
            counts[slot] = count;
            if count != 0 {
                queue.push_back(slot);
                pending[slot] = true;
            }
        }
        while let Some(child) = queue.pop_front() {
            pending[child] = false;
            for &(class, sibling) in &self.parents[child] {
                let count = counts[child].saturating_add(counts[sibling]).min(required);
                if count > counts[class] {
                    counts[class] = count;
                    if !pending[class] {
                        pending[class] = true;
                        queue.push_back(class);
                    }
                }
            }
        }
        counts
    }
}

struct AlgebraicSearch {
    relation: Relation,
    available: Option<Vec<usize>>,
    witnesses: Option<AlgebraicTable>,
}

impl AlgebraicSearch {
    fn new<L: Label>(
        eg: &Engine<L>,
        label: LabelId,
        selectors: &[Var],
        associative: bool,
        accepts: &dyn Fn(Var, ClassId) -> bool,
    ) -> Self {
        let relation = Relation::new(eg, label);
        let available = associative.then(|| relation.availability(selectors, accepts));
        Self {
            relation,
            available,
            witnesses: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn at(
        &mut self,
        root: ClassId,
        selectors: &[Var],
        remainder: bool,
        associative: bool,
        row_bound: bool,
        accepts: &dyn Fn(Var, ClassId) -> bool,
        orders: &[(usize, usize)],
    ) -> Vec<(RowId, Witness)> {
        let root = self.relation.slots[root.index()];
        if self
            .available
            .as_ref()
            .is_some_and(|counts| counts[root] < selectors.len())
        {
            return Vec::new();
        }
        let witnesses = self.witnesses.get_or_insert_with(|| {
            AlgebraicTable::build(&self.relation, selectors, associative, accepts, orders)
        });
        witnesses.at(
            root,
            remainder,
            associative && !row_bound,
            &self.relation.rows,
            orders,
        )
    }
}

struct AlgebraicTable {
    states: Vec<States>,
    repeated: Vec<(Var, Vec<usize>)>,
    arena: ProofArena,
}

/// Upper bounds over all finite same-label witnesses. Alternative rows sum
/// rather than choose, so this may retain an impossible selector but never
/// removes a possible one. Saturation at the required occurrence count only
/// discards count distinctions that cannot affect matching.
fn occurrence_bounds(relation: &Relation, required: usize) -> Vec<usize> {
    let mut bounds = vec![0usize; relation.classes.len()];
    let mut queue = std::collections::VecDeque::new();
    for (class, parents) in relation.parents.iter().enumerate() {
        if parents.is_empty() {
            bounds[class] = 1;
            queue.push_back((class, 1));
        }
    }
    loop {
        while let Some((class, delta)) = queue.pop_front() {
            for &(_, left, right) in &relation.rows[class] {
                for child in [left, right] {
                    let prior = bounds[child];
                    let next = prior.saturating_add(delta).min(required);
                    if next > prior {
                        bounds[child] = next;
                        queue.push_back((child, next - prior));
                    }
                }
            }
        }
        // Classes unreached from maximal roots have cyclic ancestry. Treat
        // those cycles and their descendants as potentially repeated.
        let unseen: Vec<_> = bounds
            .iter()
            .enumerate()
            .filter_map(|(class, &count)| (count == 0).then_some(class))
            .collect();
        if unseen.is_empty() {
            break;
        }
        for class in unseen {
            bounds[class] = required;
            queue.push_back((class, required));
        }
    }
    bounds
}

impl AlgebraicTable {
    fn build(
        relation: &Relation,
        selectors: &[Var],
        associative: bool,
        accepts: &dyn Fn(Var, ClassId) -> bool,
        orders: &[(usize, usize)],
    ) -> Self {
        let mut repeated: BTreeMap<Var, Vec<usize>> = BTreeMap::new();
        for (slot, &var) in selectors.iter().enumerate() {
            repeated.entry(var).or_default().push(slot);
        }
        let repeated: Vec<_> = repeated
            .into_iter()
            .filter(|(_, slots)| slots.len() > 1)
            .collect();
        let Relation {
            classes, parents, ..
        } = relation;
        let multiplicities: BTreeMap<_, _> = repeated
            .iter()
            .map(|(var, slots)| (*var, slots.len()))
            .collect();
        let bounds = multiplicities
            .values()
            .max()
            .map(|&required| occurrence_bounds(relation, required));
        let mut arena = ProofBuilder::default();
        let mut states: Vec<_> = classes.iter().map(|_| States::default()).collect();
        let mut queue = std::collections::VecDeque::new();
        for (class_slot, &class) in classes.iter().enumerate() {
            let state = &mut states[class_slot];
            // Zero selectors keep an existing class opaque, without unfolding
            // alternative spellings of the unmatched context.
            let opaque = StateWitness {
                bindings: vec![None; selectors.len()],
                residual: Some(ProofRef::Class(class)),
                cost: 0,
            };
            let mut seeds = vec![opaque];
            for (slot, &var) in selectors.iter().enumerate() {
                if multiplicities
                    .get(&var)
                    .is_some_and(|&required| bounds.as_ref().unwrap()[class_slot] < required)
                {
                    continue;
                }
                if accepts(var, class) {
                    let mut bindings = vec![None; selectors.len()];
                    bindings[slot] = Some(class);
                    seeds.push(StateWitness {
                        bindings,
                        residual: None,
                        cost: 0,
                    });
                }
            }
            for seed in seeds {
                let id = state.retain(seed, &repeated).unwrap();
                if associative {
                    state.records[id].queued = true;
                    queue.push_back((class_slot, id));
                }
            }
        }
        // A state has a fixed class, selector assignment, and remainder
        // presence. Positive row cost makes its least finite witness stable,
        // including in cyclic classes. Only changed states revisit parents.
        while let Some((child, id)) = queue.pop_front() {
            let state = &mut states[child].records[id];
            state.queued = false;
            let witness = state.witness.clone();
            for &(class, sibling) in &parents[child] {
                // An opaque sibling on a self-loop preserves these bindings
                // and an already-present remainder, but adds positive cost.
                // The existing incoming state therefore dominates it.
                let self_context = class == child && witness.residual.is_some();
                if self_context && witness.bindings.iter().all(Option::is_some) {
                    continue;
                }
                for id in states[sibling].compatible(&witness, &repeated, false) {
                    let other = &states[sibling].records[id].witness;
                    if self_context && other.bindings.iter().all(Option::is_none) {
                        continue;
                    }
                    let Some(bindings) = combined_bindings(&witness, other, &repeated) else {
                        continue;
                    };
                    if !ordered(&bindings, orders) {
                        continue;
                    }
                    let cost = combined_cost(&witness, other);
                    let residual = witness.residual.is_some() || other.residual.is_some();
                    if !states[class].admits(&bindings, residual, cost) {
                        continue;
                    }
                    let combined = StateWitness {
                        bindings,
                        residual: arena.combine(witness.residual, other.residual),
                        cost,
                    };
                    let state = &mut states[class];
                    if let Some(id) = state.retain(combined, &repeated) {
                        let record = &mut state.records[id];
                        if !record.queued {
                            record.queued = true;
                            queue.push_back((class, id));
                        }
                    }
                }
            }
        }
        Self {
            states,
            repeated,
            arena: arena.freeze(),
        }
    }

    fn at(
        &self,
        root: usize,
        remainder: bool,
        reuse_root: bool,
        rows: &[Vec<(RowId, usize, usize)>],
        orders: &[(usize, usize)],
    ) -> Vec<(RowId, Witness)> {
        if reuse_root {
            let row = rows[root][0].0;
            // A positive-cost complete state came from a concrete operator row
            // at this class. A zero-cost singleton seed only selected the class
            // opaquely and is not a root operation match. Type and attributes
            // are identical across this table's concrete-label rows.
            let state = &self.states[root];
            return state
                .entries
                .values()
                .map(|&id| &state.records[id].witness)
                .filter(|witness| {
                    witness.cost > 0
                        && witness.bindings.iter().all(Option::is_some)
                        && remainder == witness.residual.is_some()
                })
                .cloned()
                .map(|witness| (row, witness.owned(&self.arena)))
                .collect();
        }
        let mut out = Vec::new();
        for &(row, left, right) in &rows[root] {
            let mut found: BTreeMap<Vec<Option<ClassId>>, Witness> = BTreeMap::new();
            let left = &self.states[left];
            let right = &self.states[right];
            for &id in left.entries.values() {
                let a = &left.records[id].witness;
                for id in right.compatible(a, &self.repeated, true) {
                    let b = &right.records[id].witness;
                    let Some(bindings) = combined_bindings(a, b, &self.repeated) else {
                        continue;
                    };
                    if !ordered(&bindings, orders) {
                        continue;
                    }
                    let cost = combined_cost(a, b);
                    let root = match (a.residual, b.residual) {
                        (Some(a), Some(b)) => Some(ProofRoot::Join(a, b)),
                        (a, b) => a.or(b).map(ProofRoot::Ref),
                    };
                    if bindings.iter().any(Option::is_none)
                        || remainder != root.is_some()
                        || found.get(&bindings).is_some_and(|prior| prior.cost <= cost)
                    {
                        continue;
                    }
                    found.insert(
                        bindings.clone(),
                        Witness {
                            bindings,
                            residual: root.map(|root| ResidualExpr {
                                arena: self.arena.clone(),
                                root,
                            }),
                            cost,
                        },
                    );
                }
            }
            out.extend(found.into_values().map(|witness| (row, witness)));
        }
        out
    }
}

/// Enumerate occurrence partitions once for a certified AC identity. Existing
/// classes with finite subbag proofs remain selector candidates, so selecting
/// an intermediate operation does not require a physical parent spelling.
#[allow(clippy::too_many_arguments)]
fn ground_witnesses<L: Label>(
    eg: &Engine<L>,
    root: ClassId,
    label: LabelId,
    selectors: &[Var],
    remainder: bool,
    accepts: &dyn Fn(Var, ClassId) -> bool,
    orders: &[(usize, usize)],
    cache: &mut AlgebraicCache,
) -> Option<(bool, Vec<(RowId, Witness)>)> {
    use crate::identity::GroundKey;
    let identity = eg.algebraic_identity();
    let GroundKey::Bag(root_key) = identity.certified.get(&(label, root))? else {
        return None;
    };
    let search = cache
        .ground
        .entry(label)
        .or_insert_with(|| GroundSearch::new(eg, label))
        .as_ref()?;
    let row = *search.rows.get(&root)?;
    let mut counts: BTreeMap<_, _> = root_key.iter().copied().collect();
    let mut candidates: Vec<_> = root_key
        .iter()
        .flat_map(|(atom, _)| search.anchors.get(atom).into_iter().flatten().copied())
        .collect();
    candidates.sort_unstable();
    candidates.retain(|&candidate| {
        search.candidates[candidate]
            .1
            .iter()
            .all(|&(atom, count)| counts.get(&atom).copied().unwrap_or(0) >= count)
    });
    let choices: Vec<Vec<_>> = selectors
        .iter()
        .map(|&var| {
            candidates
                .iter()
                .copied()
                .filter(|&candidate| accepts(var, search.candidates[candidate].0))
                .collect()
        })
        .collect();
    struct Partitions<'a> {
        selectors: &'a [Var],
        orders: &'a [(usize, usize)],
        choices: &'a [Vec<usize>],
        candidates: &'a [(ClassId, Vec<(ClassId, usize)>)],
        remainder: bool,
        arena: ProofBuilder,
        out: Vec<StateWitness>,
    }
    impl Partitions<'_> {
        fn visit(
            &mut self,
            index: usize,
            counts: &mut BTreeMap<ClassId, usize>,
            bindings: &mut Vec<Option<ClassId>>,
        ) {
            if index == self.selectors.len() {
                if self.remainder != counts.values().any(|&count| count != 0) {
                    return;
                }
                self.out.push(StateWitness {
                    bindings: bindings.clone(),
                    cost: self.selectors.len(),
                    residual: self.arena.bag(counts),
                });
                return;
            }
            let var = self.selectors[index];
            for &candidate in &self.choices[index] {
                let (class, key) = &self.candidates[candidate];
                let class = *class;
                if self.selectors[..index]
                    .iter()
                    .zip(bindings.iter())
                    .any(|(&prior, &bound)| prior == var && bound != Some(class))
                    || key
                        .iter()
                        .any(|&(atom, count)| counts.get(&atom).copied().unwrap_or(0) < count)
                {
                    continue;
                }
                for &(atom, count) in key {
                    *counts.get_mut(&atom).unwrap() -= count;
                }
                bindings.push(Some(class));
                if ordered(bindings, self.orders) {
                    self.visit(index + 1, counts, bindings);
                }
                bindings.pop();
                for &(atom, count) in key {
                    *counts.get_mut(&atom).unwrap() += count;
                }
            }
        }
    }
    let mut partitions = Partitions {
        selectors,
        orders,
        choices: &choices,
        candidates: &search.candidates,
        remainder,
        arena: ProofBuilder::default(),
        out: Vec::new(),
    };
    partitions.visit(0, &mut counts, &mut Vec::new());
    let complete = search
        .unresolved
        .iter()
        .all(|&class| selectors.iter().all(|&var| !accepts(var, class)));
    let arena = partitions.arena.freeze();
    Some((
        complete,
        partitions
            .out
            .into_iter()
            .map(|witness| (row, witness.owned(&arena)))
            .collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn witnesses<L: Label>(
    eg: &Engine<L>,
    root: ClassId,
    template: &L,
    selectors: &[Var],
    remainder: bool,
    associative: bool,
    commutative: bool,
    row_bound: bool,
    accepts: &dyn Fn(Var, ClassId) -> bool,
    orders: &[(usize, usize)],
    cache: &mut AlgebraicCache,
) -> Vec<(RowId, LabelId, Witness)> {
    let mut out = Vec::new();
    let mut labels = BTreeSet::new();
    let root = eg.find(root);
    for &label in eg.labels_with_op(template.op_key()) {
        let Some(node) = eg.label_node(label) else {
            continue;
        };
        if node.children().len() != 2
            || !template.matches_template(node)
            || (associative && (!node.associative() || !node.commutative()))
            || (commutative && !node.commutative())
        {
            continue;
        }
        if associative
            && commutative
            && !row_bound
            && !selectors.is_empty()
            && let Some((complete, found)) = ground_witnesses(
                eg, root, label, selectors, remainder, accepts, orders, cache,
            )
        {
            out.extend(
                found
                    .into_iter()
                    .map(|(row, witness)| (row, label, witness)),
            );
            if complete {
                continue;
            }
        }
        let rows: Vec<_> = eg
            .rows(root)
            .filter(|&row| eg.label(row) == label)
            .collect();
        if selectors.is_empty() {
            if remainder {
                for row in rows {
                    out.push((
                        row,
                        label,
                        Witness {
                            bindings: Vec::new(),
                            residual: Some(ResidualExpr {
                                arena: Arc::from([]),
                                root: ProofRoot::Ref(ProofRef::Class(root)),
                            }),
                            cost: 1,
                        },
                    ));
                }
            }
        } else if !rows.is_empty() {
            labels.insert(label);
        }
    }
    for label in labels {
        let table = cache
            .tables
            .entry(label)
            .or_insert_with(|| AlgebraicSearch::new(eg, label, selectors, associative, accepts));
        out.extend(
            table
                .at(
                    eg.find(root),
                    selectors,
                    remainder,
                    associative,
                    row_bound,
                    accepts,
                    orders,
                )
                .into_iter()
                .map(|(row, witness)| (row, label, witness)),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Term;
    use crate::{HeadOp, LabelFill, NoExterns, Plan, Query, Rule};

    fn add(left: ClassId, right: ClassId) -> Term {
        Term::comm("add", &[left, right])
    }

    fn materialized_remainder(
        eg: &mut Engine<Term>,
        matched: &crate::Match,
        atoms: &[ClassId],
    ) -> (BTreeMap<ClassId, u128>, usize) {
        fn count(
            eg: &Engine<Term>,
            class: ClassId,
            atoms: &BTreeSet<ClassId>,
            memo: &mut BTreeMap<ClassId, BTreeMap<ClassId, u128>>,
        ) -> BTreeMap<ClassId, u128> {
            let class = eg.find(class);
            if atoms.contains(&class) {
                return BTreeMap::from([(class, 1)]);
            }
            if let Some(known) = memo.get(&class) {
                return known.clone();
            }
            let node = eg
                .nodes(class)
                .find(|node| node.op == "add")
                .expect("a remainder consists of original atoms and add rows");
            let mut result = count(eg, node.children[0], atoms, memo);
            for (atom, occurrences) in count(eg, node.children[1], atoms, memo) {
                *result.entry(atom).or_default() += occurrences;
            }
            memo.insert(class, result.clone());
            result
        }
        eg.push_context();
        let marker = eg.add(Term::leaf("residual-result"));
        let mut matched = matched.clone();
        let target = matched.bindings.len() as Var;
        matched.bindings.push(Some(marker));
        let size = eg.total_size();
        eg.apply_head(
            &[HeadOp::Union(target, matched.residuals[0].var)],
            0,
            &matched,
        );
        let added = eg.total_size() - size;
        let atoms = atoms.iter().map(|&class| eg.find(class)).collect();
        let result = count(eg, marker, &atoms, &mut BTreeMap::new());
        eg.pop_context();
        (result, added)
    }

    fn identity_rule(selectors: &[Var]) -> Rule<Term> {
        Rule {
            name: "identity-test".into(),
            plan: Plan::compile(Query::tree(
                selectors.iter().copied().max().unwrap_or(0) + 1,
                0,
                vec![Atom::Algebraic {
                    template: add(ClassId(0), ClassId(0)),
                    selectors: SmallVec::from_slice(selectors),
                    remainder: None,
                    class: 0,
                    row: None,
                    associative: true,
                    commutative: true,
                    remainder_type: None,
                }],
            )),
            head: Vec::new(),
            head_vars: 0,
            post_saturation: false,
        }
    }

    fn permutations(items: &[ClassId]) -> Vec<Vec<ClassId>> {
        if items.is_empty() {
            return vec![Vec::new()];
        }
        let mut out = Vec::new();
        for index in 0..items.len() {
            let mut rest = items.to_vec();
            let first = rest.remove(index);
            for mut tail in permutations(&rest) {
                tail.insert(0, first);
                out.push(tail);
            }
        }
        out
    }

    fn bracketings(eg: &mut Engine<Term>, leaves: &[ClassId], all: bool) -> Vec<ClassId> {
        if leaves.len() == 1 {
            return vec![leaves[0]];
        }
        let mut out = Vec::new();
        for split in 1..leaves.len() {
            let left = bracketings(eg, &leaves[..split], all);
            let right = bracketings(eg, &leaves[split..], all);
            for &left in &left {
                for &right in &right {
                    out.push(eg.add(add(left, right)));
                }
            }
            if !all {
                break;
            }
        }
        out
    }

    // Protects the owner boundary, not just query equivalence: binary spelling
    // must cease allocating distinct semantic roots, without losing bindings.
    #[test]
    fn normalized_identity_preserves_all_ordered_bindings_and_binary_provenance() {
        for (preseed, all) in [(true, true), (false, true), (true, false)] {
            let mut eg = Engine::new();
            let rule = identity_rule(&[1, 2, 3, 4]);
            if preseed {
                eg.register_algebraic_rules(std::slice::from_ref(&rule));
            }
            let leaves: Vec<_> = ["a", "b", "c", "d"]
                .map(|name| eg.add(Term::leaf(name)))
                .into();
            let expected: BTreeSet<_> = permutations(&leaves).into_iter().collect();
            let mut roots = Vec::new();
            for permutation in &expected {
                roots.extend(bracketings(&mut eg, permutation, all));
            }
            assert_eq!(roots.len(), if all { 120 } else { 24 });
            if !preseed {
                assert_eq!(roots.iter().copied().collect::<BTreeSet<_>>().len(), 120);
                eg.register_algebraic_rules(std::slice::from_ref(&rule));
            }
            eg.rebuild();
            let root = eg.find(roots[0]);
            assert!(roots.iter().all(|&other| eg.connected(root, other)));
            let label = eg.label(eg.rows(root).next().unwrap());
            let root_keys: Vec<_> = eg
                .algebraic_identity()
                .certified
                .iter()
                .filter(|((family, owner), _)| *family == label && *owner == root)
                .collect();
            assert_eq!(root_keys.len(), 1);
            assert_eq!(
                eg.algebraic_identity()
                    .proofs
                    .keys()
                    .filter(|(family, _)| *family == label)
                    .count(),
                11
            );
            assert!(eg.rows(root).count() > 1);
            let found = rule
                .plan
                .search(&eg, [root], &|_, _| true, false, &NoExterns);
            let actual: BTreeSet<_> = found
                .iter()
                .map(|matched| {
                    matched.bindings[1..]
                        .iter()
                        .map(|id| id.unwrap())
                        .collect::<Vec<_>>()
                })
                .collect();
            assert_eq!(found.len(), 24);
            assert_eq!(actual, expected);
            let intermediate = eg.lookup(&add(leaves[0], leaves[1])).unwrap();
            let two = identity_rule(&[1, 2]);
            let found = two
                .plan
                .search(&eg, [root], &|_, _| true, false, &NoExterns);
            assert!(
                found
                    .iter()
                    .any(|matched| matched.bindings[1] == Some(intermediate))
            );
        }
    }

    #[test]
    fn class_order_matches_filtered_ordered_bindings() {
        for certified in [false, true] {
            for cyclic in [false, true] {
                for row_bound in [false, true] {
                    for selectors in [&[1, 2][..], &[1, 1, 2][..]] {
                        let mut eg = Engine::new();
                        let rule = identity_rule(selectors);
                        if certified {
                            eg.register_algebraic_rules(std::slice::from_ref(&rule));
                        }
                        let a = eg.add(Term::leaf("a"));
                        let b = eg.add(Term::leaf("b"));
                        let aa = eg.add(add(a, a));
                        let root = eg.add(add(aa, b));
                        if cyclic {
                            let recursive = eg.add(add(root, a));
                            eg.union(root, recursive);
                        }
                        eg.rebuild();
                        let mut query = rule.plan.query().clone();
                        if row_bound {
                            query.scalars = 1;
                            let Atom::Algebraic { row, .. } = &mut query.atoms[0] else {
                                unreachable!()
                            };
                            *row = Some(0);
                        }
                        for asymmetric in [false, true] {
                            let accepts = |var, class| !asymmetric || var != 1 || class == b;
                            let unguarded = Plan::compile(query.clone()).search(
                                &eg,
                                [root],
                                &accepts,
                                false,
                                &NoExterns,
                            );
                            assert!(!unguarded.is_empty() || selectors.len() == 3 && asymmetric);
                            let expected: BTreeSet<_> = unguarded
                                .iter()
                                .filter(|m| m.bindings[1] <= m.bindings[2])
                                .map(|m| (m.bindings.clone(), m.scalars.clone()))
                                .collect();
                            let mut ordered = query.clone();
                            ordered.guards.push(crate::Guard::ClassLe(1, 2));
                            let actual: BTreeSet<_> = Plan::compile(ordered)
                                .search(&eg, [root], &accepts, false, &NoExterns)
                                .into_iter()
                                .map(|m| (m.bindings, m.scalars))
                                .collect();
                            assert_eq!(actual, expected);
                            if selectors.len() == 2 && !asymmetric {
                                assert!(unguarded.iter().any(|m| m.bindings[1] > m.bindings[2]));
                                assert!(!actual.is_empty());
                            }
                        }
                    }
                }
            }
        }
    }

    // A merge changes bag multiplicities, while a popped scope must restore
    // both normalized identities and registration made inside the scope.
    #[test]
    fn algebraic_identity_repairs_atoms_and_rolls_back_registration() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let aa = eg.add(add(a, a));
        let ab = eg.add(add(a, b));
        let ba = eg.add(add(b, a));
        let rule = identity_rule(&[1, 2]);
        eg.push_context();
        eg.register_algebraic_rules(std::slice::from_ref(&rule));
        assert!(eg.connected(ab, ba));
        eg.union(a, b);
        eg.rebuild();
        assert!(eg.connected(aa, ab));
        eg.pop_context();
        assert!(!eg.connected(ab, ba));
        assert!(!eg.connected(aa, ab));
        assert!(eg.algebraic_identity().policies.is_empty());
        eg.register_algebraic_rules(std::slice::from_ref(&rule));
        assert!(eg.connected(ab, ba));
        assert!(!eg.connected(aa, ab));
        eg.union(a, b);
        eg.rebuild();
        assert!(eg.connected(aa, ab));
    }

    // A cyclic alternative revokes flattening certification while previously
    // proved finite identities and finite query witnesses remain valid.
    #[test]
    fn algebraic_identity_keeps_proofs_when_a_class_becomes_cyclic() {
        let mut eg = Engine::new();
        let rule = identity_rule(&[1, 2]);
        eg.register_algebraic_rules(std::slice::from_ref(&rule));
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let ab = eg.add(add(a, b));
        let ba = eg.add(add(b, a));
        assert_eq!(ab, ba);
        let recursive = eg.add(add(ab, b));
        eg.union(ab, recursive);
        eg.rebuild();
        assert!(
            !eg.algebraic_identity()
                .certified
                .keys()
                .any(|&(_, class)| class == eg.find(ab))
        );
        assert_eq!(eg.lookup(&add(b, a)), Some(eg.find(ab)));
        let found = rule.plan.search(&eg, [ab], &|_, _| true, false, &NoExterns);
        assert!(
            found
                .iter()
                .any(|matched| matched.bindings[1] == Some(a) && matched.bindings[2] == Some(b))
        );
    }

    #[test]
    fn registration_applies_only_the_requested_laws() {
        for associative in [false, true] {
            let mut eg = Engine::new();
            let mut rule = identity_rule(&[1, 2]);
            let mut query = rule.plan.query().clone();
            if let Atom::Algebraic {
                associative: a,
                commutative: c,
                ..
            } = &mut query.atoms[0]
            {
                *a = associative;
                *c = !associative;
            }
            rule.plan = Plan::compile(query);
            eg.register_algebraic_rules(&[rule]);
            let a = eg.add(Term::leaf("a"));
            let b = eg.add(Term::leaf("b"));
            let c = eg.add(Term::leaf("c"));
            let ab = eg.add(add(a, b));
            let ba = eg.add(add(b, a));
            assert_eq!(eg.connected(ab, ba), !associative);
            let bc = eg.add(add(b, c));
            let left = eg.add(add(ab, c));
            let right = eg.add(add(a, bc));
            eg.rebuild();
            assert_eq!(eg.connected(left, right), associative);
            let opaque = eg.add(Term::leaf("opaque"));
            eg.union(ab, opaque);
            eg.rebuild();
            let label = eg.label(eg.rows(ab).next().unwrap());
            assert!(
                !eg.algebraic_identity()
                    .certified
                    .contains_key(&(label, eg.find(ab)))
            );
            assert_eq!(eg.lookup(&add(a, b)), Some(eg.find(ab)));
        }
    }

    #[test]
    fn registered_queries_preserve_large_multiplicities_and_compact_remainders() {
        let mut eg = Engine::new();
        let exact = identity_rule(&[1; 65]);
        eg.register_algebraic_rules(std::slice::from_ref(&exact));
        let a = eg.add(Term::leaf("a"));
        let mut root = a;
        for _ in 1..65 {
            root = eg.add(add(root, a));
        }
        eg.rebuild();
        let accepts = |var, class| var != 1 || class == a;
        let found = exact.plan.search(&eg, [root], &accepts, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bindings[1], Some(a));

        let mut eg = Engine::new();
        eg.register_algebraic_rules(std::slice::from_ref(&exact));
        let a = eg.add(Term::leaf("a"));
        let accepts = |var, class| var != 1 || class == a;
        let remainder = Plan::compile(Query::tree(
            3,
            0,
            vec![Atom::Algebraic {
                template: add(ClassId(0), ClassId(0)),
                selectors: SmallVec::from_slice(&[1]),
                remainder: Some(2),
                class: 0,
                row: None,
                associative: true,
                commutative: true,
                remainder_type: None,
            }],
        ));
        let mut query = remainder.query().clone();
        if let Atom::Algebraic { selectors, .. } = &mut query.atoms[0] {
            *selectors = SmallVec::from_slice(&[1, 1]);
        }
        let remainder = Plan::compile(query);
        let mut root = a;
        for exponent in 1..=usize::BITS + 8 {
            root = eg.add(add(root, root));
            if exponent != 40 && exponent != usize::BITS && exponent != usize::BITS + 8 {
                continue;
            }
            eg.rebuild();
            let size = eg.total_size();
            let found = remainder.search(&eg, [root], &accepts, false, &NoExterns);
            assert_eq!(found.len(), 1);
            assert_eq!(eg.total_size(), size);
            let (counts, added) = materialized_remainder(&mut eg, &found[0], &[a]);
            assert_eq!(counts, BTreeMap::from([(a, (1u128 << exponent) - 2)]));
            assert!(added <= 2 * exponent as usize);
        }
    }

    #[test]
    fn mixed_alternatives_preserve_selector_coverage_and_finite_remainders() {
        let mut eg = Engine::new();
        eg.register_algebraic_rules(&[identity_rule(&[1, 2])]);
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let c = eg.add(Term::leaf("c"));
        let mixed = eg.add(add(a, b));
        let seven = eg.add(Term::int(7));
        eg.union(mixed, seven);
        let root = eg.add(add(mixed, c));
        let x = eg.add(Term::leaf("x"));
        let three = eg.add(Term::int(3));
        let five = eg.add(Term::int(5));
        let inner = eg.add(add(x, three));
        let unrelated = eg.add(add(inner, five));
        let remote = eg.add(add(x, five));
        eg.rebuild();
        let size = eg.total_size();
        let query = Query::tree(
            3,
            0,
            vec![Atom::Algebraic {
                template: add(ClassId(0), ClassId(0)),
                selectors: SmallVec::from_slice(&[1]),
                remainder: Some(2),
                class: 0,
                row: None,
                associative: true,
                commutative: true,
                remainder_type: None,
            }],
        );
        let plan = Plan::compile(query);
        // The remote class is a proved subbag, but not a physical subtree of
        // this root. An unrelated mixed class must not disable that selection.
        let found = plan.search(
            &eg,
            [unrelated],
            &|var, class| var != 1 || class == remote,
            false,
            &NoExterns,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bindings[1], Some(remote));
        assert_eq!(
            materialized_remainder(&mut eg, &found[0], &[three]).0,
            BTreeMap::from([(three, 1)])
        );

        let found = plan.search(&eg, [root], &|_, _| true, false, &NoExterns);
        let mixed = eg.find(mixed);
        assert_eq!(
            found
                .iter()
                .map(|matched| matched.bindings[1].unwrap())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([a, b, c, mixed])
        );
        for matched in &found {
            let selected = matched.bindings[1].unwrap();
            let expected = if selected == mixed {
                BTreeMap::from([(c, 1)])
            } else if selected == c {
                BTreeMap::from([(mixed, 1)])
            } else {
                BTreeMap::from([(if selected == a { b } else { a }, 1), (c, 1)])
            };
            assert_eq!(
                materialized_remainder(&mut eg, matched, &[a, b, c, mixed]).0,
                expected
            );
        }
        assert_eq!(eg.total_size(), size);
    }

    fn constants() -> Plan<Term> {
        let mut atoms = vec![
            Atom::Node {
                template: add(ClassId(0), ClassId(0)),
                args: SmallVec::from_slice(&[4, 2]),
                class: 0,
                row: None,
            },
            Atom::Node {
                template: add(ClassId(0), ClassId(0)),
                args: SmallVec::from_slice(&[3, 1]),
                class: 4,
                row: None,
            },
            Atom::Literal {
                value: Term::int(3),
                class: 1,
            },
            Atom::Literal {
                value: Term::int(5),
                class: 2,
            },
        ];
        lower_algebraic(
            &mut atoms,
            0,
            Some(3),
            AlgebraicOptions {
                associative: true,
                commutative: false,
            },
            None,
        )
        .unwrap();
        Plan::compile(Query::tree(5, 0, atoms))
    }

    // Protects reassociation, preserved context, delayed construction, and
    // canonical class references when an earlier accepted head merged a leaf.
    #[test]
    fn selects_across_groupings_and_builds_only_the_used_remainder() {
        let mut eg = Engine::new();
        let x = eg.add(Term::leaf("x"));
        let y = eg.add(Term::leaf("y"));
        let three = eg.add(Term::int(3));
        let five = eg.add(Term::int(5));
        let left = eg.add(add(x, three));
        let right = eg.add(add(y, five));
        let root = eg.add(add(left, right));
        let larger = eg.add(add(root, x));
        eg.rebuild();
        let size = eg.total_size();
        let plan = constants();
        for roots in [[root, larger], [larger, root]] {
            let found = plan.search(&eg, roots, &|_, _| true, false, &NoExterns);
            assert_eq!(found.len(), 2);
            for (found, expected_root) in found.iter().zip(roots) {
                assert_eq!(found.root, expected_root);
                assert_eq!(found.bindings[1], Some(three));
                assert_eq!(found.bindings[2], Some(five));
                assert_eq!(
                    materialized_remainder(&mut eg, found, &[x, y]).0,
                    BTreeMap::from([(x, if expected_root == root { 1 } else { 2 }), (y, 1)])
                );
            }
        }
        let found = plan.search(&eg, [root], &|_, _| true, false, &NoExterns);
        assert_eq!(found[0].bindings[1], Some(three));
        assert_eq!(found[0].bindings[2], Some(five));
        assert_eq!(eg.total_size(), size);
        eg.apply_head(&[HeadOp::Union(0, 0)], 0, &found[0]);
        assert_eq!(eg.total_size(), size);
        eg.union(x, y);
        let head = [
            HeadOp::Insert {
                label: LabelFill::plain(Term::int(8)),
                args: SmallVec::new(),
                into: 5,
            },
            HeadOp::Insert {
                label: LabelFill::plain(add(ClassId(0), ClassId(0))),
                args: SmallVec::from_slice(&[3, 5]),
                into: 6,
            },
            HeadOp::Union(0, 6),
        ];
        eg.apply_head(&head, 2, &found[0]);
        eg.rebuild();
        let remainder = eg.lookup(&add(eg.find(x), eg.find(x))).unwrap();
        let eight = eg.lookup(&Term::int(8)).unwrap();
        assert_eq!(
            eg.find(root),
            eg.find(eg.lookup(&add(remainder, eight)).unwrap())
        );
        let size = eg.total_size();
        eg.apply_head(&head, 2, &found[0]);
        eg.rebuild();
        assert_eq!(eg.total_size(), size);
    }

    // One selected occurrence cannot satisfy two occurrences of a repeated
    // variable, and an exact pattern cannot silently absorb additional leaves.
    #[test]
    fn preserves_multiplicity_and_requires_a_nonempty_remainder() {
        let mut eg = Engine::new();
        let x = eg.add(Term::leaf("x"));
        let three = eg.add(Term::int(3));
        let pair = eg.add(add(three, three));
        let single = eg.add(add(x, three));
        let repeated = eg.add(add(x, pair));
        eg.rebuild();
        let atom = Atom::Algebraic {
            template: add(ClassId(0), ClassId(0)),
            selectors: SmallVec::from_slice(&[1, 1]),
            remainder: Some(2),
            class: 0,
            row: None,
            associative: true,
            commutative: true,
            remainder_type: None,
        };
        let plan = Plan::compile(Query::tree(
            3,
            0,
            vec![
                atom.clone(),
                Atom::Literal {
                    value: Term::int(3),
                    class: 1,
                },
            ],
        ));
        let found = plan.search(
            &eg,
            [single, pair, repeated],
            &|_, _| true,
            false,
            &NoExterns,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, repeated);
        let shared = eg.add(add(single, single));
        eg.rebuild();
        let found = plan.search(&eg, [shared], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(
            materialized_remainder(&mut eg, &found[0], &[x]).0,
            BTreeMap::from([(x, 2)])
        );
        let mut exact = atom;
        if let Atom::Algebraic { remainder, .. } = &mut exact {
            *remainder = None;
        }
        let plan = Plan::compile(Query::tree(
            3,
            0,
            vec![
                exact,
                Atom::Literal {
                    value: Term::int(3),
                    class: 1,
                },
            ],
        ));
        let found = plan.search(&eg, [pair, repeated], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, pair);
    }

    // Commutativity alone must still match swapped binary operands, while
    // retaining the operation's grouping across a nested occurrence.
    #[test]
    fn commutative_only_matches_binary_rows() {
        let mut eg = Engine::new();
        let y = eg.add(Term::leaf("y"));
        let x = eg.add(Term::leaf("x"));
        let z = eg.add(Term::leaf("z"));
        let binary = eg.add(add(y, x));
        let nested = eg.add(add(binary, z));
        eg.rebuild();
        let plan = Plan::compile(Query::tree(
            3,
            0,
            vec![
                Atom::Algebraic {
                    template: add(ClassId(0), ClassId(0)),
                    selectors: SmallVec::from_slice(&[1, 2]),
                    remainder: None,
                    class: 0,
                    row: None,
                    associative: false,
                    commutative: true,
                    remainder_type: None,
                },
                Atom::Node {
                    template: Term::leaf("x"),
                    args: SmallVec::new(),
                    class: 1,
                    row: None,
                },
            ],
        ));
        let found = plan.search(&eg, [nested, binary], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, binary);
        assert_eq!(found[0].bindings[1], Some(x));
        assert_eq!(found[0].bindings[2], Some(y));
    }

    // Root admission facts can guard an algebraic query without reading its
    // deferred remainder. Scoped constants must suppress only that scope.
    #[test]
    fn negated_root_constant_guard_admits_deferred_remainders() {
        let mut eg = Engine::new();
        let x = eg.add(Term::leaf("x"));
        let three = eg.add(Term::int(3));
        let five = eg.add(Term::int(5));
        let inner = eg.add(add(x, three));
        let root = eg.add(add(inner, five));
        eg.rebuild();
        let mut query = constants().query().clone();
        query.scalars = 1;
        query.nots.push(crate::Nested {
            atoms: vec![Atom::Fact {
                column: crate::ColumnId::Const,
                key: 0,
                value: 0,
            }],
            guards: Vec::new(),
        });
        let plan = Plan::compile(query.clone());
        assert_eq!(
            plan.search(&eg, [root], &|_, _| true, false, &NoExterns)
                .len(),
            1
        );
        eg.push_context();
        eg.assume_const(root, Term::int(8));
        eg.rebuild();
        assert!(
            plan.search(&eg, [root], &|_, _| true, false, &NoExterns)
                .is_empty()
        );
        eg.pop_context();
        assert_eq!(
            plan.search(&eg, [root], &|_, _| true, false, &NoExterns)
                .len(),
            1
        );
        query.nots[0].atoms = vec![Atom::Fact {
            column: crate::ColumnId::Const,
            key: 3,
            value: 0,
        }];
        assert!(std::panic::catch_unwind(|| Plan::compile(query)).is_err());
    }

    // A selected constant can become available through a merge many levels
    // below the root, after the first round. Scoped facts must not outlive pop.
    #[test]
    fn rematches_deep_changes_and_forgets_scoped_facts() {
        let mut eg = Engine::new();
        let unknown = eg.add(Term::leaf("unknown"));
        let x = eg.add(Term::leaf("x"));
        let three = eg.add(Term::int(3));
        let five = eg.add(Term::int(5));
        let mut root = eg.add(add(unknown, three));
        for _ in 0..12 {
            root = eg.add(add(root, x));
        }
        eg.rebuild();
        let plan = constants();
        assert!(
            plan.search(&eg, [root], &|_, _| true, false, &NoExterns)
                .is_empty()
        );
        eg.push_context();
        eg.assume_const(unknown, Term::int(5));
        eg.rebuild();
        assert_eq!(
            plan.search(&eg, [root], &|_, _| true, true, &NoExterns)
                .len(),
            1
        );
        eg.pop_context();
        assert!(
            plan.search(&eg, [root], &|_, _| true, false, &NoExterns)
                .is_empty()
        );
        let marker = eg.add(Term::leaf("matched"));
        let merge = Rule {
            name: "reveal-five".into(),
            plan: Plan::compile(Query::tree(
                3,
                0,
                vec![
                    Atom::Node {
                        template: add(ClassId(0), ClassId(0)),
                        args: SmallVec::from_slice(&[1, 2]),
                        class: 0,
                        row: None,
                    },
                    Atom::Literal {
                        value: Term::int(3),
                        class: 2,
                    },
                ],
            )),
            head: vec![
                HeadOp::Insert {
                    label: LabelFill::plain(Term::int(5)),
                    args: SmallVec::new(),
                    into: 3,
                },
                HeadOp::Union(1, 3),
            ],
            head_vars: 1,
            post_saturation: false,
        };
        let matched = Rule {
            name: "find-deep-five".into(),
            plan,
            head: vec![
                HeadOp::Insert {
                    label: LabelFill::plain(Term::leaf("matched")),
                    args: SmallVec::new(),
                    into: 5,
                },
                HeadOp::Union(0, 5),
            ],
            head_vars: 1,
            post_saturation: false,
        };
        eg.saturate_rules(&[merge, matched], &NoExterns, 8, 1000);
        assert_eq!(eg.find(unknown), eg.find(five));
        assert_eq!(eg.find(root), eg.find(marker));
    }

    // Cyclic alternatives require a finite witness search, rather than a
    // recursion cutoff that loses a match whose selected leaf is in the cycle.
    #[test]
    fn finds_a_finite_witness_in_a_cyclic_class() {
        let mut eg = Engine::new();
        let x = eg.add(Term::leaf("x"));
        let three = eg.add(Term::int(3));
        let five = eg.add(Term::int(5));
        let cycle = eg.add(add(x, three));
        eg.union(cycle, x);
        let y = eg.add(Term::leaf("y"));
        let z = eg.add(Term::leaf("z"));
        let unavailable = eg.add(add(y, z));
        eg.union(unavailable, y);
        eg.rebuild();
        let repeated = Plan::compile(Query::tree(
            3,
            0,
            vec![
                Atom::Algebraic {
                    template: add(ClassId(0), ClassId(0)),
                    selectors: SmallVec::from_slice(&[1, 1]),
                    remainder: Some(2),
                    class: 0,
                    row: None,
                    associative: true,
                    commutative: true,
                    remainder_type: None,
                },
                Atom::Literal {
                    value: Term::int(3),
                    class: 1,
                },
            ],
        ));
        // A first root with no accepted occurrence must not suppress a later
        // root whose cyclic finite witness supplies both selected occurrences.
        let found = repeated.search(&eg, [y, x], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        let expected = eg.find(x);
        assert_eq!(
            materialized_remainder(&mut eg, &found[0], &[x]).0,
            BTreeMap::from([(expected, 1)])
        );
        let root = eg.add(add(x, five));
        eg.rebuild();
        let size = eg.total_size();
        let found = constants().search(&eg, [root], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        let expected = eg.find(x);
        assert_eq!(
            materialized_remainder(&mut eg, &found[0], &[x]).0,
            BTreeMap::from([(expected, 1)])
        );
        assert_eq!(eg.total_size(), size);
    }
    // No pair of occurrences exists in a distinct operand chain. Searching
    // all its roots must preserve that absence and leave graph size unchanged.
    #[test]
    fn repeated_selector_does_not_match_distinct_operand_chains() {
        let mut eg = Engine::new();
        let leaves: Vec<_> = (0..256)
            .map(|i| eg.add(Term::leaf(&format!("x{i}"))))
            .collect();
        let mut root = leaves[0];
        for &leaf in &leaves[1..] {
            root = eg.add(add(root, leaf));
        }
        eg.rebuild();
        let plan = Plan::compile(Query::tree(
            3,
            0,
            vec![Atom::Algebraic {
                template: add(ClassId(0), ClassId(0)),
                selectors: SmallVec::from_slice(&[1, 1]),
                remainder: Some(2),
                class: 0,
                row: None,
                associative: true,
                commutative: true,
                remainder_type: None,
            }],
        ));
        let size = eg.total_size();
        assert!(
            plan.search(&eg, plan.roots(&eg), &|_, _| true, false, &NoExterns)
                .is_empty()
        );
        assert_eq!(eg.total_size(), size);
    }

    #[derive(Clone, Debug)]
    struct Typed {
        term: Term,
        bits: u64,
        attribute: u64,
    }

    impl Label for Typed {
        fn children(&self) -> &[ClassId] {
            self.term.children()
        }
        fn children_mut(&mut self) -> &mut [ClassId] {
            self.term.children_mut()
        }
        fn hash_cons(&self) -> u64 {
            self.term.hash_cons() ^ self.bits.rotate_left(7) ^ self.attribute.rotate_left(13)
        }
        fn op_key(&self) -> u64 {
            self.term.op_key()
        }
        fn matches(&self, other: &Self) -> bool {
            self.term.matches(&other.term)
                && self.bits == other.bits
                && self.attribute == other.attribute
        }
        fn matches_template(&self, other: &Self) -> bool {
            self.term.matches(&other.term)
                && (self.bits == 0 || self.bits == other.bits)
                && (self.attribute == 0 || self.attribute == other.attribute)
        }
        fn constant(&self) -> Option<Self> {
            self.term.constant().map(|term| Typed {
                term,
                bits: 0,
                attribute: 0,
            })
        }
        fn associative(&self) -> bool {
            self.term.associative()
        }
        fn commutative(&self) -> bool {
            self.term.commutative()
        }
        fn type_key(&self) -> Option<u64> {
            (self.bits != 0).then_some(self.bits)
        }
        fn is_unique(&self) -> bool {
            self.attribute == u64::MAX
        }
    }

    #[test]
    fn registered_identity_keeps_exact_labels_and_unique_nodes_separate() {
        let typed = |term, bits, attribute| Typed {
            term,
            bits,
            attribute,
        };
        let mut eg = Engine::new();
        let a = eg.add(typed(Term::leaf("a"), 8, 0));
        let b = eg.add(typed(Term::leaf("b"), 8, 0));
        let plan = Plan::compile(Query::tree(
            3,
            0,
            vec![Atom::Algebraic {
                template: typed(add(ClassId(0), ClassId(0)), 0, 0),
                selectors: SmallVec::from_slice(&[1, 2]),
                remainder: None,
                class: 0,
                row: None,
                associative: true,
                commutative: true,
                remainder_type: None,
            }],
        ));
        eg.register_algebraic_rules(&[Rule {
            name: "typed-identity".into(),
            plan,
            head: Vec::new(),
            head_vars: 0,
            post_saturation: false,
        }]);
        let mut owners = Vec::new();
        for (bits, attribute) in [(8, 7), (16, 7), (8, 9)] {
            let ab = eg.add(typed(add(a, b), bits, attribute));
            let ba = eg.add(typed(add(b, a), bits, attribute));
            assert_eq!(ab, ba);
            assert!(owners.iter().all(|&other| !eg.connected(ab, other)));
            owners.push(ab);
        }
        let first = eg.add(typed(add(a, b), 8, u64::MAX));
        let second = eg.add(typed(add(a, b), 8, u64::MAX));
        eg.rebuild();
        assert!(!eg.connected(first, second));

        // Shared atoms can affect arbitrarily many exact families. Scoped
        // merges must restore their dependencies for a later base merge.
        let c = eg.add(typed(Term::leaf("c"), 8, 0));
        let mut regroupings = Vec::new();
        for attribute in 10..22 {
            let ab = eg.add(typed(add(a, b), 8, attribute));
            let left = eg.add(typed(add(ab, c), 8, attribute));
            let bc = eg.add(typed(add(b, c), 8, attribute));
            let right = eg.add(typed(add(b, bc), 8, attribute));
            assert!(!eg.connected(left, right));
            regroupings.push((left, right));
        }
        eg.push_context();
        eg.union(a, b);
        eg.rebuild();
        for &(left, right) in &regroupings {
            assert!(eg.connected(left, right));
        }
        eg.pop_context();
        for &(left, right) in &regroupings {
            assert!(!eg.connected(left, right));
        }
        eg.union(a, b);
        eg.rebuild();
        for (index, &(left, right)) in regroupings.iter().enumerate() {
            assert!(eg.connected(left, right));
            for &(other, _) in &regroupings[..index] {
                assert!(!eg.connected(left, other));
            }
        }
    }

    // A wildcard root template must not erase concrete type or attribute
    // boundaries while searching, and residual types come from that root row.
    #[test]
    fn respects_concrete_labels_under_a_wildcard_template() {
        let typed = |term, bits, attribute| Typed {
            term,
            bits,
            attribute,
        };
        let mut eg = Engine::new();
        let x = eg.add(typed(Term::leaf("x"), 8, 0));
        let y = eg.add(typed(Term::leaf("y"), 8, 0));
        let three = eg.add(typed(Term::int(3), 0, 0));
        let five = eg.add(typed(Term::int(5), 0, 0));
        let right = eg.add(typed(add(y, five), 8, 7));
        let correct = eg.add(typed(add(x, three), 8, 7));
        let other_type = eg.add(typed(add(x, three), 16, 7));
        let other_attr = eg.add(typed(add(x, three), 8, 9));
        let correct = eg.add(typed(add(correct, right), 8, 7));
        let other_type = eg.add(typed(add(other_type, right), 8, 7));
        let other_attr = eg.add(typed(add(other_attr, right), 8, 7));
        eg.rebuild();
        let plan = Plan::compile(Query {
            vars: 4,
            scalars: 1,
            root: 0,
            atoms: vec![
                Atom::Algebraic {
                    template: typed(add(ClassId(0), ClassId(0)), 0, 0),
                    selectors: SmallVec::from_slice(&[1, 2]),
                    remainder: Some(3),
                    class: 0,
                    row: None,
                    associative: true,
                    commutative: true,
                    remainder_type: Some(0),
                },
                Atom::Literal {
                    value: typed(Term::int(3), 0, 0),
                    class: 1,
                },
                Atom::Literal {
                    value: typed(Term::int(5), 0, 0),
                    class: 2,
                },
            ],
            guards: Vec::new(),
            nots: Vec::new(),
        });
        let found = plan.search(
            &eg,
            [correct, other_type, other_attr],
            &|_, _| true,
            false,
            &NoExterns,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, correct);
        assert_eq!(found[0].scalars.as_slice(), &[8]);
    }
}
