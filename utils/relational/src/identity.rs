//! Finite algebraic identities. Certificates permit flattening; stored proofs
//! remain valid when a later union gives a class another realization.

use std::collections::{BTreeMap, BTreeSet};

use crate::{AlgebraicOptions, ClassId, Engine, Label, LabelId, RowId};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GroundKey {
    Pair([ClassId; 2]),
    Sequence(Vec<ClassId>),
    Bag(Vec<(ClassId, usize)>),
}

impl GroundKey {
    pub(crate) fn atom(class: ClassId, laws: AlgebraicOptions) -> Self {
        if laws.commutative {
            Self::Bag(vec![(class, 1)])
        } else {
            Self::Sequence(vec![class])
        }
    }

    pub(crate) fn combine(left: Self, right: Self, laws: AlgebraicOptions) -> Option<Self> {
        if !laws.associative {
            return None;
        }
        match (left, right) {
            (Self::Sequence(mut left), Self::Sequence(right)) => {
                left.len().checked_add(right.len())?;
                left.try_reserve(right.len()).ok()?;
                left.extend(right);
                Some(Self::Sequence(left))
            }
            (Self::Bag(left), Self::Bag(right)) => {
                let mut counts = Vec::with_capacity(left.len().checked_add(right.len())?);
                let mut left = left.into_iter().peekable();
                let mut right = right.into_iter().peekable();
                while let (Some(&(left_class, left_count)), Some(&(right_class, right_count))) =
                    (left.peek(), right.peek())
                {
                    match left_class.cmp(&right_class) {
                        std::cmp::Ordering::Less => counts.push(left.next()?),
                        std::cmp::Ordering::Greater => counts.push(right.next()?),
                        std::cmp::Ordering::Equal => {
                            counts.push((left_class, left_count.checked_add(right_count)?));
                            left.next();
                            right.next();
                        }
                    }
                }
                counts.extend(left);
                counts.extend(right);
                Some(Self::Bag(counts))
            }
            _ => None,
        }
    }

    pub(crate) fn transport<L: Label>(&self, eg: &Engine<L>) -> Option<Self> {
        match self {
            Self::Pair(children) => {
                let mut children = children.map(|child| eg.find(child));
                children.sort_unstable();
                Some(Self::Pair(children))
            }
            Self::Sequence(children) => Some(Self::Sequence(
                children.iter().map(|&child| eg.find(child)).collect(),
            )),
            Self::Bag(children) => {
                let mut counts: Vec<_> = children
                    .iter()
                    .map(|&(child, count)| (eg.find(child), count))
                    .collect();
                counts.sort_unstable_by_key(|&(class, _)| class);
                let mut len = 0;
                for index in 0..counts.len() {
                    let (class, count) = counts[index];
                    if len > 0 && counts[len - 1].0 == class {
                        counts[len - 1].1 = counts[len - 1].1.checked_add(count)?;
                    } else {
                        counts[len] = (class, count);
                        len += 1;
                    }
                }
                counts.truncate(len);
                Some(Self::Bag(counts))
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Identity<L> {
    pub(crate) dirty: BTreeSet<LabelId>,
    // Proof atoms come from physical children, including through flattened
    // family productions. Keep these watches after certificate revocation.
    pub(crate) occurrences: BTreeMap<ClassId, smallvec::SmallVec<[LabelId; 2]>>,
    pub(crate) resolved: usize,
    pub(crate) requests: Vec<(L, AlgebraicOptions)>,
    pub(crate) policies: BTreeMap<LabelId, AlgebraicOptions>,
    pub(crate) proofs: BTreeMap<(LabelId, GroundKey), ClassId>,
    pub(crate) certified: BTreeMap<(LabelId, ClassId), GroundKey>,
}

impl<L> Default for Identity<L> {
    fn default() -> Self {
        Self {
            dirty: BTreeSet::new(),
            occurrences: BTreeMap::new(),
            resolved: 0,
            requests: Vec::new(),
            policies: BTreeMap::new(),
            proofs: BTreeMap::new(),
            certified: BTreeMap::new(),
        }
    }
}

impl<L> Identity<L> {
    pub(crate) fn watch(&mut self, label: LabelId, classes: impl IntoIterator<Item = ClassId>) {
        for class in classes {
            let families = self.occurrences.entry(class).or_default();
            if let Err(index) = families.binary_search(&label) {
                families.insert(index, label);
            }
        }
    }

    pub(crate) fn merge(&mut self, survivor: ClassId, absorbed: ClassId) {
        if let Some(families) = self.occurrences.remove(&absorbed) {
            for label in families {
                self.watch(label, [survivor]);
            }
        }
        if let Some(families) = self.occurrences.get(&survivor) {
            self.dirty.extend(families.iter().copied());
        }
    }
}

pub(crate) struct Certificates {
    pub(crate) keys: BTreeMap<ClassId, Option<GroundKey>>,
    pub(crate) productions: BTreeMap<ClassId, Vec<(RowId, [ClassId; 2])>>,
}

/// Bottom-up certification leaves recursive components unresolved. A class
/// with a non-family alternative cannot flatten, even if it has a finite exit.
pub(crate) fn certificates<L: Label>(
    eg: &Engine<L>,
    label: LabelId,
    laws: AlgebraicOptions,
) -> Certificates {
    struct Pending {
        dependencies: BTreeSet<ClassId>,
        mixed: bool,
    }
    let mut productions = BTreeMap::<ClassId, Vec<(RowId, [ClassId; 2])>>::new();
    if let Some(table) = eg.table(label) {
        for (offset, &found) in table.column(0).iter().enumerate() {
            if found != label.0 {
                continue;
            }
            let class = eg.find(ClassId(table.column(table.arity())[offset]));
            let row = RowId(table.tags()[offset]);
            let children = eg.children(row);
            productions
                .entry(class)
                .or_default()
                .push((row, [children[0], children[1]]));
        }
    }
    let mut pending = BTreeMap::new();
    let mut keys = BTreeMap::new();
    for (&class, rows) in &productions {
        let dependencies = if laws.associative {
            rows.iter()
                .flat_map(|(_, children)| children.iter().copied())
                .collect()
        } else {
            BTreeSet::new()
        };
        pending.insert(
            class,
            Pending {
                dependencies,
                mixed: eg.rows(class).any(|row| eg.label(row) != label),
            },
        );
    }
    let mut parents = BTreeMap::<ClassId, Vec<ClassId>>::new();
    let mut remaining = BTreeMap::new();
    let mut queue = std::collections::VecDeque::new();
    for (&class, production) in &pending {
        let unresolved: Vec<_> = production
            .dependencies
            .iter()
            .filter(|child| pending.contains_key(child))
            .copied()
            .collect();
        remaining.insert(class, unresolved.len());
        if unresolved.is_empty() {
            queue.push_back(class);
        }
        for child in unresolved {
            parents.entry(child).or_default().push(class);
        }
    }
    while let Some(class) = queue.pop_front() {
        let production = &pending[&class];
        let mut common = None;
        let mut valid = !production.mixed;
        if valid {
            for &(_, children) in &productions[&class] {
                let key = if laws.associative {
                    let [left, right] = children.map(|child| {
                        keys.get(&child)
                            .cloned()
                            .unwrap_or_else(|| Some(GroundKey::atom(child, laws)))
                    });
                    left.zip(right)
                        .and_then(|(left, right)| GroundKey::combine(left, right, laws))
                } else {
                    let mut children = children;
                    children.sort_unstable();
                    Some(GroundKey::Pair(children))
                };
                match key {
                    Some(key) if common.as_ref().is_none_or(|common| *common == key) => {
                        common = Some(key)
                    }
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
        }
        keys.insert(class, valid.then_some(common).flatten());
        for &parent in parents.get(&class).into_iter().flatten() {
            let count = remaining.get_mut(&parent).unwrap();
            *count -= 1;
            if *count == 0 {
                queue.push_back(parent);
            }
        }
    }
    for class in pending.keys() {
        keys.entry(*class).or_insert(None);
    }
    Certificates { keys, productions }
}

pub(crate) fn production_key<L: Label>(
    eg: &Engine<L>,
    label: LabelId,
    laws: AlgebraicOptions,
    children: [ClassId; 2],
) -> Option<GroundKey> {
    let children = children.map(|child| eg.find(child));
    if !laws.associative {
        let mut children = children;
        children.sort_unstable();
        return Some(GroundKey::Pair(children));
    }
    let identity = eg.algebraic_identity();
    let [left, right] = children.map(|child| {
        (!identity.dirty.contains(&label))
            .then(|| identity.certified.get(&(label, child)).cloned())
            .flatten()
            .unwrap_or_else(|| GroundKey::atom(child, laws))
    });
    GroundKey::combine(left, right, laws)
}
