use std::cell::RefCell;
use std::marker::PhantomData;

use smallvec::SmallVec;

use tir_relational::query::Resolution;
use tir_relational::rule::Key;
use tir_relational::saturate::Caches;
use tir_relational::store::{Repair, RowView, Table};
use tir_relational::unionfind::{Merge, UnionFind};
use tir_relational::{Database, Join, LatticeId, SpaceId, Stats};

use crate::label::{Carrier, Labels, Laws};
use crate::{ClassId, ColumnId, FxHashMap, Label, LabelId, Ref, RowId};

/// Empty link in an intrusive list.
const NONE: u32 = u32::MAX;

/// A label bit: a commutative binary operator.
const COMMUTES: u8 = 1;
/// A label bit: no carrier, so no laws, and a key of classes alone.
const PLAIN: u8 = 2;
/// A label bit: a leaf integer constant of a carrier, which is a reference
/// and never a row.
const CONSTANT: u8 = 4;

/// The id space classes are minted in, the database's only one.
pub(crate) const CLASSES: SpaceId = SpaceId::nth(0);

/// The constant a class is known to be, as a label: seeded by every literal
/// row outside a carrier, raised by a scope's assumption, joined by a union.
///
/// The lattice columns are declared in the order a search from a risen fact
/// tries them, which is how rarely they rise: a constant, then a type, which
/// nearly every new class is minted with.
pub(crate) const CONSTS: LatticeId = LatticeId::nth(0);
/// The type a class's terms carry, seeded by every typed row. Congruence
/// already forces a class's rows to agree on it, so the first row to say wins
/// and a merge does not make the answer depend on merge order.
pub(crate) const TYPES: LatticeId = LatticeId::nth(1);

/// A union of two references that name one class at different offsets, or
/// two carriers' zeros. Nothing was merged; [`Engine::contradicted`] reports
/// it until the scope it happened in is popped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contradiction;

/// What a node comes to before its table is probed.
pub(crate) enum Shaped {
    /// Its laws settle it without a row.
    Ref(Ref),
    /// The key is built; the offset is what the row's value is shifted by.
    Key(u64),
}

/// `node`'s carrier and value, when it is an integer constant of one.
pub(crate) fn leaf_constant<L: Label>(node: &L) -> Option<(Carrier, u64)> {
    let carrier = node.carrier()?;
    let value = node.int_value()?;
    Some((
        carrier,
        value & tir_relational::unionfind::mask(carrier.width),
    ))
}

/// What a node with `laws` over `operands` is without a row, if its laws
/// settle it: an identity element at one operand makes it the other, and a
/// linear node over constants is the constant the coefficients sum them to.
/// `operands` are canonical; one with a coefficient may carry its offset or
/// have had it moved out, as long as the caller applies the same convention to
/// the answer.
fn fold(uf: &UnionFind, zeros: &[(u64, ClassId)], laws: &Laws, operands: &[Ref]) -> Option<Ref> {
    for (at, identity) in laws.identity.iter().enumerate() {
        let (Some(identity), Some(&element)) = (identity, operands.get(at)) else {
            continue;
        };
        if !uf.is_zero(element.class.0) {
            continue;
        }
        // op(x, zero + identity) == x, and a coefficient carries the rest.
        let shift = match laws.coefficient(at) {
            0 if element.offset == *identity => 0,
            0 => continue,
            coefficient => {
                (coefficient as u64).wrapping_mul(element.offset.wrapping_sub(*identity))
            }
        };
        let other = operands[1 - at];
        // An operand of another carrier cannot take the shift.
        if uf.mask(other.class.0) == laws.mask {
            return Some(Ref::new(
                other.class,
                other.offset.wrapping_add(shift) & laws.mask,
            ));
        }
    }
    if laws.linear() && operands.iter().all(|operand| uf.is_zero(operand.class.0)) {
        let key = laws.carrier?.key;
        let zero = zeros.iter().find(|&&(known, _)| known == key)?.1;
        let sum = operands
            .iter()
            .enumerate()
            .fold(laws.bias, |sum, (at, operand)| {
                sum.wrapping_add((laws.coefficient(at) as u64).wrapping_mul(operand.offset))
            });
        return Some(Ref::new(zero, sum & laws.mask));
    }
    None
}

/// What a table holds: rows of one arity, hash-consed or not, whose operands
/// carry these offset coefficients. Labels that agree on it share a table.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shape {
    arity: usize,
    unique: bool,
    /// Empty when no operand has one.
    coefficients: SmallVec<[i64; 2]>,
}

/// The e-graph: equivalence classes of terms, as a client of a [`Database`].
///
/// Classes are the ids of one space of the database, and a value is a
/// [`Ref`], a class plus the offset the database's union-find keeps. Each
/// label has a table keyed on the operands and valued by the reference the
/// node is, with the offsets the label's laws move to the result factored out
/// of the key, so hash-consing is a key lookup and congruence is the table's
/// functional dependency: a rebuild canonicalizes the tables and merges the
/// values of the rows that come to share a key. A rule's atoms are joins over
/// those tables.
///
/// What the tables do not hold is the term itself. A row is tagged with the
/// e-node it was interned as, and `node` keeps that term verbatim, provenance
/// included. Its inline children are canonical *as of the insert* and drift
/// afterwards, so every reader passes them through [`Self::find`]. A class
/// lists its e-nodes in insertion order, a union appending the absorbed class's
/// after the survivor's; an e-node a rebuild found congruent to an older one
/// leaves its table but stays on that list, since it may carry provenance the
/// older one does not.
pub struct Engine<L: Label> {
    /// The tables, the class partition, the class lists and the facts:
    /// everything a scope undoes.
    pub(crate) db: Database<Lists, 1>,
    labels: Labels<L>,

    /// The term each e-node was interned as, its label, and the class it was
    /// minted with: the node's value is that class's reference.
    node: Vec<L>,
    node_label: Vec<LabelId>,
    node_class: Vec<ClassId>,
    /// Per e-node, where its operands' offsets start in `operand_offsets`, or
    /// [`NONE`] when every one is zero. Only an operand without a coefficient
    /// keeps one.
    node_offsets: Vec<u32>,
    operand_offsets: Vec<u64>,
    /// [`Label::op_key`] -> the labels under it, in the order they were first
    /// seen.
    op_labels: FxHashMap<u64, Vec<LabelId>>,
    /// Per label, the table its rows live in, the [`COMMUTES`], [`PLAIN`]
    /// and [`CONSTANT`] bits an insert branches on, and its offset laws.
    label_slot: Vec<u32>,
    label_flags: Vec<u8>,
    label_op: Vec<u64>,
    label_laws: Vec<Laws>,
    /// What each table slot holds, in the order first needed.
    shapes: Vec<Shape>,
    /// Whether some label has an identity or is linear, so a rebuild looks
    /// for rows its laws now settle.
    reshapes: bool,

    /// Per open scope, the classes it found distinct, grouped under the class
    /// it merged them into; merged groups only. Starts as the enclosing
    /// scope's, so a nested scope keeps naming base classes.
    members: Vec<FxHashMap<ClassId, Vec<ClassId>>>,
    /// Per open scope, how many operand offsets the e-nodes it found keep.
    offsets: Vec<usize>,

    repair: Repair,
    /// Per plan, what its searches keep, and which rules a round visits for
    /// the rule set last saturated with. Labels only accumulate, so an entry
    /// stays valid for the engine's life.
    pub(crate) caches: RefCell<Caches>,
    /// Scratch for the variables a rule head binds.
    pub(crate) head_bound: Vec<tir_relational::Ref>,
}

/// The e-nodes of each class as an intrusive list and the carriers' zeros,
/// kept in the database so a scope undoes them.
#[derive(Clone, Default)]
pub(crate) struct Lists {
    /// Per e-node, the next e-node of its class, or [`NONE`].
    next: Vec<u32>,
    /// Per class, its first and last e-node and how many it has.
    head: Vec<u32>,
    tail: Vec<u32>,
    size: Vec<u32>,
    /// Each carrier's zero class, by carrier key, in the order minted.
    zeros: Vec<(u64, ClassId)>,
}

impl<L: Label> Default for Engine<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: Label> Engine<L> {
    pub fn new() -> Self {
        let mut db = Database::new();
        let facts = [
            db.add_lattice(CLASSES, Join::Agree),
            db.add_lattice(CLASSES, Join::First),
        ];
        debug_assert_eq!(facts, [CONSTS, TYPES]);
        Self {
            db,
            labels: Labels::default(),
            node: Vec::new(),
            node_label: Vec::new(),
            node_class: Vec::new(),
            node_offsets: Vec::new(),
            operand_offsets: Vec::new(),
            op_labels: FxHashMap::default(),
            label_slot: Vec::new(),
            label_flags: Vec::new(),
            label_op: Vec::new(),
            label_laws: Vec::new(),
            shapes: Vec::new(),
            reshapes: false,
            members: Vec::new(),
            offsets: Vec::new(),
            repair: Repair::default(),
            caches: RefCell::default(),
            head_bound: Vec::new(),
        }
    }

    // ---- reading ----------------------------------------------------------

    /// `reference` as its class's representative plus an offset.
    #[inline]
    pub fn find(&self, reference: impl Into<Ref>) -> Ref {
        self.db.find(CLASSES, reference.into()).into()
    }

    /// The representative of `id`'s class, whatever offset `id` sits at from
    /// it.
    #[inline]
    pub fn root(&self, id: ClassId) -> ClassId {
        ClassId(self.db.root(CLASSES, id.0))
    }

    /// Whether `a` and `b` are the same value.
    pub fn connected(&self, a: impl Into<Ref>, b: impl Into<Ref>) -> bool {
        self.find(a) == self.find(b)
    }

    /// Whether a union since the outermost open scope (or ever, with none
    /// open) was a [`Contradiction`]: under an assumption, that the
    /// assumption is impossible.
    pub fn contradicted(&self) -> bool {
        self.db.contradicted(CLASSES)
    }

    fn uf(&self) -> &UnionFind {
        self.db.partition(CLASSES)
    }

    pub fn is_empty(&self) -> bool {
        self.num_classes() == 0
    }

    /// Total e-nodes across all classes. An e-node found congruent to another
    /// still counts, so this only ever grows within a context — the fixpoint
    /// test both saturation drivers use reads it.
    pub fn total_size(&self) -> usize {
        self.node.len()
    }

    pub fn num_classes(&self) -> usize {
        self.db.live(CLASSES)
    }

    /// One past the highest class id ever minted, live or not — the size a table
    /// indexed by class id needs.
    pub fn class_count(&self) -> usize {
        self.db.ids(CLASSES)
    }

    /// Bytes the e-nodes, the tables and the per-class arrays hold, without the
    /// tables' key indexes. An estimate for ranking, not an allocator total.
    pub fn approx_bytes(&self) -> usize {
        let cells: usize = self
            .db
            .tables()
            .iter()
            .map(|table| table.len() * (table.arity() + 3))
            .sum();
        self.node.len() * (size_of::<L>() + 4 * size_of::<u32>())
            + cells * size_of::<u32>()
            + self.operand_offsets.len() * size_of::<u64>()
            + self.class_count() * (5 * size_of::<u32>() + 2 * size_of::<u64>() + 1)
            + self.labels.len() * size_of::<L>()
    }

    /// Work done since the engine was built. A saturation round reads the
    /// difference across it; a scope does not roll these back, since they count
    /// work, not state.
    pub fn stats(&self) -> Stats {
        self.db.stats()
    }

    fn in_scope(&self) -> bool {
        self.db.depth() != 0
    }

    /// The operands of an e-node, canonical as of now.
    pub fn children(&self, row: RowId) -> SmallVec<[Ref; 4]> {
        let offsets = self.node_offsets[row.index()];
        self.node[row.index()]
            .children()
            .iter()
            .enumerate()
            .map(|(operand, &child)| {
                let offset = match offsets {
                    NONE => 0,
                    at => self.operand_offsets[at as usize + operand],
                };
                self.find(Ref::new(child, offset))
            })
            .collect()
    }

    /// The value of an e-node: its class's representative plus the offset the
    /// node sits at from it.
    pub fn value(&self, row: RowId) -> Ref {
        self.find(self.node_class[row.index()])
    }

    /// The operator bucket of an e-node: its label's [`Label::op_key`].
    pub(crate) fn row_op(&self, row: RowId) -> u64 {
        self.label_op[self.node_label[row.index()].index()]
    }

    pub fn label(&self, row: RowId) -> LabelId {
        self.node_label[row.index()]
    }

    pub fn node(&self, row: RowId) -> &L {
        &self.node[row.index()]
    }

    /// The e-nodes of `id`'s class in insertion order, a union appending the
    /// absorbed class's after the survivor's. Extraction breaks ties by this
    /// order, so it is part of the contract — a rebuild must not re-sort it.
    pub fn rows(&self, id: ClassId) -> Rows<'_, L> {
        let lists = self.db.client();
        Rows {
            next: &lists.next,
            cursor: lists.head[self.root(id).index()],
            label: PhantomData,
        }
    }

    /// E-nodes of `id`'s class; child ids may be non-canonical — resolve with
    /// [`Self::find`].
    pub fn nodes(&self, id: ClassId) -> impl Iterator<Item = &L> + Clone {
        self.rows(id).map(|row| &self.node[row.index()])
    }

    fn class_len(&self, id: ClassId) -> usize {
        self.db.client().size[self.root(id).index()] as usize
    }

    /// Every live class, ascending. A class is named by its lowest member, so
    /// under a scope this is the position of the group's lowest base class.
    /// Extraction and bare-variable-rooted patterns walk classes in this order
    /// and break ties by it, so it is part of the contract.
    pub fn class_ids(&self) -> impl Iterator<Item = ClassId> + '_ {
        let size = &self.db.client().size;
        (0..self.class_count() as u32)
            .map(ClassId)
            .filter(move |&id| self.root(id) == id && size[id.index()] != 0)
    }

    /// [`Self::class_ids`] as readable classes.
    pub fn classes(&self) -> impl Iterator<Item = ClassRef<'_, L>> + '_ {
        self.class_ids().map(|id| self.class(id))
    }

    pub fn class(&self, id: ClassId) -> ClassRef<'_, L> {
        ClassRef {
            engine: self,
            id: self.root(id),
        }
    }

    /// Whether `label` is a commutative binary operator.
    pub(crate) fn commutes(&self, label: LabelId) -> bool {
        self.label_flags[label.index()] & COMMUTES != 0
    }

    /// The labels whose operator bucket is `op`, in the order first seen.
    pub(crate) fn labels_with_op(&self, op: u64) -> &[LabelId] {
        self.op_labels.get(&op).map_or(&[], Vec::as_slice)
    }

    /// The labels of the `op` bucket that `keep` accepts, ascending: all of
    /// them, or with `since` those met from that count on.
    fn labels_in(&self, op: u64, since: Option<usize>, keep: impl FnMut(LabelId)) {
        let labels = self.labels_with_op(op);
        // A bucket lists its labels in the order met, so ascending.
        let from = since.map_or(0, |since| {
            labels.partition_point(|label| label.index() < since)
        });
        labels[from..].iter().copied().for_each(keep);
    }

    /// Add to `out` the labels in the `op` bucket that `template` matches at
    /// its arity, by the table their rows live in; with `commuting`, only the
    /// commutative binary ones. Tables come in the order of their lowest such
    /// label. With `since`, only the labels met from that count on are looked
    /// at, and `out` already holds the rest.
    pub(crate) fn labels_matching(
        &self,
        op: u64,
        (template, arity): (&L, usize),
        commuting: bool,
        since: Option<usize>,
        out: &mut Resolution,
    ) {
        let tables = &mut out.tables;
        self.labels_in(op, since, |label| {
            let node = self.labels.node(label);
            let matches = node.children().len() == arity && template.matches_template(node);
            if !matches || commuting && !self.commutes(label) {
                return;
            }
            let slot = self.label_slot[label.index()];
            // Labels arrive ascending, so each table's list stays sorted.
            match tables.iter_mut().find(|(known, _)| *known == slot) {
                Some((_, labels)) => labels.push(label.0),
                None => tables.push((slot, SmallVec::from_slice(&[label.0]))),
            }
        });
    }

    /// Add to `out` the labels in the `op` bucket that `value` matches,
    /// ascending; `since` as for [`Self::labels_matching`].
    pub(crate) fn labels_equal(
        &self,
        op: u64,
        value: &L,
        since: Option<usize>,
        out: &mut Resolution,
    ) {
        let cells = &mut out.cells;
        self.labels_in(op, since, |label| {
            if value.matches(self.labels.node(label)) {
                cells.push(label.0);
            }
        });
    }

    /// The table `label`'s rows live in.
    pub(crate) fn label_slot(&self, label: LabelId) -> u32 {
        self.label_slot[label.index()]
    }

    /// How many labels the graph has met. What a plan's atoms read is fixed
    /// between two readings that agree.
    pub(crate) fn labels_len(&self) -> usize {
        self.labels.len()
    }

    /// Canonical classes holding a node in the `op` bucket, each once, in the
    /// order their labels were first seen. Over-approximates — callers confirm
    /// with the label.
    pub fn classes_with_op(&self, op: u64) -> Vec<ClassId> {
        let labels = self.labels_with_op(op);
        for &label in labels {
            self.db.prepare_group(self.label_slot[label.index()], 0);
        }
        let groups = self.db.groups();
        let mut seen = self.db.marks(CLASSES);
        let mut out = Vec::new();
        for &label in labels {
            let slot = self.label_slot[label.index()];
            let Some(table) = self.db.table(slot) else {
                continue;
            };
            for &row in groups.get(slot, 0).rows(label.0) {
                let root = self.root(ClassId(table.values()[row as usize]));
                if seen.insert(root.index()) {
                    out.push(root);
                }
            }
        }
        out
    }

    /// The value of an already-interned `node` over its children at offset
    /// zero, or `None` (never inserts; always `None` for a unique node).
    pub fn lookup(&self, node: &L) -> Option<Ref> {
        let operands: SmallVec<[Ref; 4]> = node.children().iter().map(|&c| c.into()).collect();
        self.lookup_at(node, &operands)
    }

    /// [`Self::lookup`] over `operands` in place of `node`'s children.
    pub fn lookup_at(&self, node: &L, operands: &[Ref]) -> Option<Ref> {
        if operands.is_empty()
            && let Some((carrier, value)) = leaf_constant(node)
        {
            return Some(Ref::new(self.zero(carrier.key)?, value));
        }
        let label = self.labels.get(node)?;
        let operands: SmallVec<[Ref; 4]> = operands.iter().map(|&o| self.find(o)).collect();
        let mut key = Key::default();
        match self.shape(label, &operands, &mut key) {
            Shaped::Ref(found) => Some(found),
            Shaped::Key(sum) => {
                let found = self.probe(label, &key)?;
                Some(self.find(Ref::new(found.class, found.offset.wrapping_add(sum))))
            }
        }
    }

    /// The row holding `key` in `label`'s table, as the value it stores.
    fn probe(&self, label: LabelId, key: &Key) -> Option<Ref> {
        let table = self.db.table(*self.label_slot.get(label.index())?)?;
        let row = table.get(&key.cells, key.weights())?;
        Some(Ref::new(
            ClassId(table.values()[row as usize]),
            table.weight(table.arity(), row),
        ))
    }

    /// What `label` over `operands`, which are canonical, comes to before a
    /// table is probed: a reference its laws settle without a row, or the
    /// key it is stored under, written into `key`, and the offset to add to
    /// that row's value.
    #[inline]
    pub(crate) fn shape(&self, label: LabelId, operands: &[Ref], key: &mut Key) -> Shaped {
        let laws = &self.label_laws[label.index()];
        if laws.mask != 0
            && let Some(found) = fold(self.uf(), &self.db.client().zeros, laws, operands)
        {
            return Shaped::Ref(found);
        }
        key.cells.push(label.0);
        key.cells
            .extend(operands.iter().map(|operand| operand.class.0));
        let mut sum = 0u64;
        if laws.coefficients.is_empty() {
            if operands.iter().any(|operand| operand.offset != 0) {
                key.weighted = true;
                key.weights.push(0);
                key.weights
                    .extend(operands.iter().map(|operand| operand.offset));
            }
            return Shaped::Key(0);
        }
        key.weights.push(0);
        for (operand, &coefficient) in operands.iter().zip(&laws.coefficients) {
            match coefficient {
                0 => {
                    key.weights.push(operand.offset);
                    key.weighted |= operand.offset != 0;
                }
                coefficient => {
                    sum = sum.wrapping_add((coefficient as u64).wrapping_mul(operand.offset));
                    key.weights.push(0);
                }
            }
        }
        Shaped::Key(sum & laws.mask)
    }

    /// The zero class of the carrier `key`, if the graph has one.
    fn zero(&self, key: u64) -> Option<ClassId> {
        let zeros = &self.db.client().zeros;
        let found = zeros.iter().find(|&&(known, _)| known == key);
        found.map(|&(_, class)| class)
    }

    /// The zero class of `carrier`, minted from `node`'s
    /// [`Label::constant_of`] if the graph has none yet.
    fn zero_or_mint(&mut self, node: &L, carrier: Carrier) -> Option<ClassId> {
        if let Some(zero) = self.zero(carrier.key) {
            return Some(zero);
        }
        let zero = node.constant_of(0)?;
        let label = self.intern(&zero);
        Some(self.make_class(zero, label, &[label.0], &[], true))
    }

    // ---- writing ----------------------------------------------------------

    /// Intern `node` over its children at offset zero, returning its value.
    pub fn add(&mut self, mut node: L) -> Ref {
        if let Some(found) = self.int_constant_ref(&node) {
            return found;
        }
        if self.uf().deltas().is_none() {
            // No class sits at an offset, so every child is its root at zero.
            for child in node.children_mut() {
                *child = ClassId(self.db.root(CLASSES, child.0));
            }
            let label = self.intern(&node);
            let operands: SmallVec<[Ref; 4]> = node.children().iter().map(|&c| c.into()).collect();
            return self.insert_canonical(label, node, &operands);
        }
        let mut operands: SmallVec<[Ref; 4]> = SmallVec::new();
        for child in node.children_mut() {
            let operand = self.find(*child);
            *child = operand.class;
            operands.push(operand);
        }
        let label = self.intern(&node);
        self.insert_canonical(label, node, &operands)
    }

    /// Intern `node` over `operands`, which stand in for its children,
    /// returning its value. A non-unique node equal to an existing one shares
    /// its class; otherwise a fresh class.
    ///
    /// The node's laws apply first: an integer constant of a carrier is its
    /// zero at the constant's value, an identity element returns the other
    /// operand, a linear node over constants is their sum, and an operand
    /// with a coefficient leaves its offset out of the key and adds it to the
    /// result instead. None of those stores a row.
    pub fn insert(&mut self, node: L, operands: &[Ref]) -> Ref {
        debug_assert_eq!(node.children().len(), operands.len());
        if let Some(found) = self.int_constant_ref(&node) {
            return found;
        }
        let mut node = node;
        let operands: SmallVec<[Ref; 4]> = operands
            .iter()
            .zip(node.children_mut())
            .map(|(&operand, child)| {
                let operand = self.find(operand);
                *child = operand.class;
                operand
            })
            .collect();
        let label = self.intern(&node);
        self.insert_canonical(label, node, &operands)
    }

    /// [`Self::insert`] of `node`, whose label is `label` and whose children
    /// are the classes of `operands`, which are canonical.
    fn insert_canonical(&mut self, label: LabelId, node: L, operands: &[Ref]) -> Ref {
        let mut key = Key::default();
        let sum = match self.shape(label, operands, &mut key) {
            Shaped::Ref(found) => return found,
            Shaped::Key(sum) => sum,
        };
        if let Some(found) = self.probe(label, &key) {
            return self.find(Ref::new(found.class, found.offset.wrapping_add(sum)));
        }
        Ref::new(
            self.make_class(node, label, &key.cells, key.weights(), false),
            sum,
        )
    }

    /// `node` as a reference when it is a leaf integer constant of a carrier
    /// that can spell its zero. A typed constant's type is what its zero's
    /// terms carry, as a row's would be its class's.
    #[inline]
    pub(crate) fn int_constant_ref(&mut self, node: &L) -> Option<Ref> {
        if !node.children().is_empty() {
            return None;
        }
        let (carrier, value) = leaf_constant(node)?;
        let zero = self.zero_or_mint(node, carrier)?;
        if let Some(key) = node.type_key() {
            self.raise_type(zero, key);
        }
        Some(Ref::new(zero, value))
    }

    /// Whether `label` is a leaf integer constant of a carrier: a reference,
    /// never a row.
    pub(crate) fn is_int_constant(&self, label: LabelId) -> bool {
        self.label_flags[label.index()] & CONSTANT != 0
    }

    /// The id of `node`'s label.
    pub(crate) fn intern(&mut self, node: &L) -> LabelId {
        let label = self.labels.intern(node);
        // Whatever the label table grew by gets its table and operator bucket.
        for index in self.label_slot.len()..self.labels.len() {
            let node = self.labels.node(LabelId(index as u32));
            let arity = node.children().len();
            let laws = Laws::of(node);
            let shape = Shape {
                arity,
                unique: node.is_unique(),
                coefficients: laws.coefficients.clone(),
            };
            let slot = match self.shapes.iter().position(|known| *known == shape) {
                Some(slot) => slot,
                None => {
                    self.shapes.push(shape);
                    self.shapes.len() - 1
                }
            };
            self.label_slot.push(slot as u32);
            let flags = [
                (arity == 2 && node.commutative(), COMMUTES),
                (laws.mask == 0, PLAIN),
                (laws.constant.is_some(), CONSTANT),
            ];
            self.label_flags.push(
                flags
                    .iter()
                    .fold(0, |bits, &(set, bit)| bits | if set { bit } else { 0 }),
            );
            let op = node.op_key();
            self.label_op.push(op);
            self.op_labels
                .entry(op)
                .or_default()
                .push(LabelId(index as u32));
            self.reshapes |= laws.identity.iter().any(Option::is_some) || laws.linear();
            self.label_laws.push(laws);
        }
        label
    }

    /// Record `a == b`, returning the representative. Congruence repair is
    /// deferred to [`Self::rebuild`], and so is everything a query reads: the
    /// tables keep naming the absorbed class until then, so a search needs a
    /// rebuild first. The merge itself is visible immediately to
    /// [`Self::find`] and [`Self::nodes`], so an applier that unions and then
    /// instantiates hash-conses against the result.
    ///
    /// `a` and `b` naming one class at different offsets is a
    /// [`Contradiction`]: nothing merges, the class is marked
    /// [`Self::conflicted`], and [`Self::contradicted`] reports it.
    pub fn union(
        &mut self,
        a: impl Into<Ref>,
        b: impl Into<Ref>,
    ) -> Result<ClassId, Contradiction> {
        let (a, b) = (a.into(), b.into());
        let (survivor, absorbed) = match self.db.equate(CLASSES, a.into(), b.into()) {
            Merge::Same => return Ok(self.root(a.class)),
            Merge::Contradiction => return Err(Contradiction),
            Merge::Merged { survivor, absorbed } => (survivor, absorbed),
        };
        self.splice_class(survivor as usize, absorbed as usize);
        let (survivor, absorbed) = (ClassId(survivor), ClassId(absorbed));
        if let Some(members) = self.members.last_mut() {
            let taken = members.remove(&absorbed).unwrap_or_else(|| vec![absorbed]);
            members
                .entry(survivor)
                .or_insert_with(|| vec![survivor])
                .extend(taken);
        }
        Ok(survivor)
    }

    /// Restore congruence to a fixpoint after a batch of unions.
    ///
    /// Each pass canonicalizes every table, carrying the offsets the merges
    /// put classes at, and merges the values of the rows that came to share a
    /// key. A re-keyed row whose laws now settle it without a row (an operand
    /// became an identity element, or every operand of a linear node a
    /// constant) leaves its table and its value is united with what it
    /// settles to. Those merges are what the next pass rewrites. Nothing
    /// depends on the order the tables are visited in or the rows within one:
    /// the class a merge leaves standing is a function of the set whichever
    /// way the merges are grouped.
    pub fn rebuild(&mut self) {
        let mut report = std::mem::take(&mut self.repair);
        let mut settled: Vec<(Ref, Ref)> = Vec::new();
        loop {
            let laws = &self.label_laws;
            let mut dissolve = |uf: &UnionFind, lists: &Lists, row: RowView<'_>| {
                let laws = &laws[row.cells[0] as usize];
                let operands: SmallVec<[Ref; 4]> = row.cells[1..]
                    .iter()
                    .zip(&row.weights[1..])
                    .map(|(&class, &offset)| Ref::new(ClassId(class), offset))
                    .collect();
                let Some(settles) = fold(uf, &lists.zeros, laws, &operands) else {
                    return false;
                };
                settled.push((Ref::new(ClassId(row.value.0), row.value.1), settles));
                true
            };
            let dissolve = self
                .reshapes
                .then_some(&mut dissolve as tir_relational::Dissolve<'_, Lists>);
            if !self.db.canonicalize(CLASSES, dissolve, &mut report) {
                break;
            }
            for collision in report.collisions.drain(..) {
                let (kept, removed) = (collision.kept, collision.removed);
                let _ = self.union(
                    Ref::new(ClassId(kept.0), kept.1),
                    Ref::new(ClassId(removed.0), removed.1),
                );
            }
            for (row, settles) in settled.drain(..) {
                let _ = self.union(row, settles);
            }
        }
        self.repair = report;
        if let Some(members) = self.members.last_mut() {
            for members in members.values_mut() {
                members.sort_unstable();
                members.dedup();
            }
        }
    }

    // ---- rows -------------------------------------------------------------

    /// Store `node` as the first row of a fresh class. `weights` is empty or
    /// one per cell of `key`; a `pinned` class is its carrier's zero.
    pub(crate) fn make_class(
        &mut self,
        node: L,
        label: LabelId,
        key: &[u32],
        weights: &[u64],
        pinned: bool,
    ) -> ClassId {
        // A carrier's constants are offsets of its zero, not facts.
        let constant = if pinned { None } else { node.constant() };
        let type_key = node.type_key();
        let laws = &self.label_laws[label.index()];
        let (mask, carrier) = (laws.mask, laws.carrier);
        let row = self.node.len() as u32;
        let class = self.db.mint(CLASSES, mask, pinned);
        let slot = self.label_slot[label.index()];
        // The table a label's rows live in is made with every table below it
        // when it gets its first row.
        while self.db.tables().len() <= slot as usize {
            let shape = &self.shapes[self.db.tables().len()];
            let columns = 1 + shape.arity;
            let table = match shape.unique {
                true => Table::bag(columns),
                false => Table::new(columns),
            };
            // The label is a key column the union-find has no say over.
            let mut table = table.plain(1);
            if !shape.coefficients.is_empty() {
                let mut coefficients = vec![0];
                coefficients.extend_from_slice(&shape.coefficients);
                table = table.coefficients(&coefficients);
            }
            self.db.add_table(table, CLASSES);
        }
        self.db.insert(slot, key, weights, (class, 0), row);
        let lists = self.db.client_mut();
        lists.next.push(NONE);
        lists.head.push(row);
        lists.tail.push(row);
        lists.size.push(1);
        if pinned {
            let carrier = carrier.expect("a zero has a carrier");
            lists.zeros.push((carrier.key, ClassId(class)));
        }
        if tir_relational::trace_enabled() {
            match weights.iter().any(|&weight| weight != 0) {
                true => eprintln!("A {} {:?} {:?} {:?}", class, node, &key[1..], &weights[1..]),
                false => eprintln!("A {} {:?} {:?}", class, node, &key[1..]),
            }
        }
        self.node.push(node);
        self.node_label.push(label);
        self.node_class.push(ClassId(class));
        match weights
            .get(1..)
            .filter(|offsets| offsets.iter().any(|&offset| offset != 0))
        {
            Some(offsets) => {
                self.node_offsets.push(self.operand_offsets.len() as u32);
                self.operand_offsets.extend_from_slice(offsets);
            }
            None => self.node_offsets.push(NONE),
        }
        if let Some(constant) = constant {
            let label = self.intern(&constant);
            self.db.seed(CONSTS, class, u64::from(label.0));
        }
        if let Some(key) = type_key {
            self.db.seed(TYPES, class, key);
        }
        ClassId(class)
    }

    /// Move the absorbed class's e-nodes onto the end of the survivor's list.
    fn splice_class(&mut self, survivor: usize, absorbed: usize) {
        let lists = self.db.client_mut();
        let head = std::mem::replace(&mut lists.head[absorbed], NONE);
        let tail = std::mem::replace(&mut lists.tail[absorbed], NONE);
        let len = std::mem::take(&mut lists.size[absorbed]);
        if head != NONE {
            match lists.tail[survivor] {
                NONE => lists.head[survivor] = head,
                survivor_tail => lists.next[survivor_tail as usize] = head,
            }
            lists.tail[survivor] = tail;
            lists.size[survivor] += len;
        }
    }

    // ---- change log -------------------------------------------------------

    /// Drain the change log: the canonical classes changed since the previous
    /// call, ascending and deduplicated; `None` means "every class".
    pub fn take_changed(&mut self) -> Option<Vec<ClassId>> {
        self.db.advance();
        let changed = self.db.take_changed(CLASSES)?;
        Some(changed.into_iter().map(ClassId).collect())
    }

    /// Report "everything" from the next [`Self::take_changed`]. A driver that
    /// stopped on a limit rather than at a fixpoint calls this: the matches it
    /// never reached are not named by the change log.
    pub fn mark_all_changed(&mut self) {
        self.db.mark_all_changed(CLASSES);
    }

    // ---- facts ------------------------------------------------------------

    /// Assume, inside the current scope, that `reference` evaluates to the
    /// constant `node`. For an integer constant of a carrier that is a union
    /// with the constant's reference, and a [`Contradiction`] refutes the
    /// scope. Any other constant is a fact in the constant column, which
    /// leaves the class's identity and parents alone. Panics with no scope
    /// open: an unscoped assumption would never be popped.
    pub fn assume_const(&mut self, reference: impl Into<Ref>, node: L) {
        assert!(
            self.in_scope(),
            "an assumption needs a scope to be undone by"
        );
        let reference = reference.into();
        if let Some(constant) = self.int_constant_ref(&node) {
            let _ = self.union(reference, constant);
            return;
        }
        let label = self.intern(&node);
        self.db
            .raise(CONSTS, self.root(reference.class).0, u64::from(label.0));
    }

    /// The integer constant `reference` is, when its class is a carrier's
    /// zero.
    pub fn int_const(&self, reference: impl Into<Ref>) -> Option<u64> {
        let found = self.find(reference);
        self.uf().is_zero(found.class.0).then_some(found.offset)
    }

    /// The width of `id`'s carrier, `None` for a class without one.
    pub fn width(&self, id: ClassId) -> Option<u32> {
        match self.uf().mask(id.0) {
            0 => None,
            mask => Some(mask.count_ones()),
        }
    }

    /// Whether a union tried to put `id`'s class at a non-zero offset from
    /// itself: two placements that disagree, which a reader takes as "placed
    /// nowhere". The class otherwise behaves as the first placement made it.
    pub fn conflicted(&self, id: ClassId) -> bool {
        self.uf().is_conflicted(id.0)
    }

    /// The constant `reference` is known to be, spelled as a node — its
    /// carrier's zero at an offset, its own literal, or what an open scope
    /// assumed of it. `None` when nothing is known and when two values were
    /// proven, which a refuted hypothesis reads as "unknown".
    pub fn const_of(&self, reference: impl Into<Ref>) -> Option<L> {
        let found = self.find(reference);
        if self.uf().is_zero(found.class.0) {
            return self.spell_constant(found);
        }
        if found.offset != 0 {
            return None;
        }
        let label = self.db.lattice(CONSTS).get(found.class.0)?;
        Some(self.labels.node(LabelId(label as u32)).clone())
    }

    /// `constant`, whose class is a carrier's zero, spelled as a node. The
    /// zero's own node heads its class: it minted it, and a union appends.
    pub fn spell_constant(&self, constant: Ref) -> Option<L> {
        let found = self.find(constant);
        let head = self.db.client().head[found.class.index()];
        self.node.get(head as usize)?.constant_of(found.offset)
    }

    /// Whether the outermost open scope found `id` a carrier's zero already.
    fn zero_before_scopes(&self, id: ClassId) -> bool {
        self.db
            .partition_before_scopes(CLASSES)
            .is_some_and(|uf| id.index() < uf.len() && uf.is_zero(id.0))
    }

    /// The constant an *open scope* assumed of `class`, as opposed to the one
    /// the class states about itself. A reader that acts on a hypothesis rather
    /// than on the program asks this.
    pub fn assumed_const(&self, class: ClassId) -> Option<L> {
        if !self.in_scope() {
            return None;
        }
        if self.int_const(class).is_some() {
            return (!self.zero_before_scopes(class))
                .then(|| self.const_of(class))
                .flatten();
        }
        let class = self.root(class);
        self.db
            .lattice(CONSTS)
            .written_in_scope(class.0)
            .then(|| self.const_of(class))
            .flatten()
    }

    /// The reference that holds `node` as its own literal, whatever type the
    /// row carries. The hash-cons cannot answer this for a constant outside a
    /// carrier: a class is known to be a *value*, and the row saying so may
    /// spell it at a type the caller has no way to name.
    pub fn const_class(&self, node: &L) -> Option<Ref> {
        if let Some((carrier, value)) = leaf_constant(node) {
            return Some(Ref::new(self.zero(carrier.key)?, value));
        }
        let consts = self.db.lattice(CONSTS);
        self.classes_with_const(node)
            .into_iter()
            .find(|&class| !consts.written_in_scope(class.0))
            .map(Ref::from)
    }

    /// The classes an open scope assumed to be `node`, ascending.
    pub fn classes_assumed_const(&self, node: &L) -> Vec<ClassId> {
        if let Some((carrier, value)) = leaf_constant(node) {
            let Some(zero) = self.zero(carrier.key) else {
                return Vec::new();
            };
            let constant = Ref::new(zero, value);
            // The scope's unions grouped what it merged under the zero.
            return self
                .scope_members(zero)
                .iter()
                .copied()
                .filter(|&member| !self.zero_before_scopes(member) && self.find(member) == constant)
                .collect();
        }
        let consts = self.db.lattice(CONSTS);
        self.classes_with_const(node)
            .into_iter()
            .filter(|&class| consts.written_in_scope(class.0))
            .collect()
    }

    /// Whether `class` was proven two different constants, or two different
    /// offsets from itself — a refuted scope.
    pub fn const_conflicted(&self, class: ClassId) -> bool {
        self.db.lattice(CONSTS).is_conflicted(self.root(class).0) || self.conflicted(class)
    }

    /// The classes known to be `node`. For an integer constant of a carrier
    /// these are every class id the union-find places at that offset from the
    /// zero, ascending; otherwise the canonical classes the constant column
    /// holds it for, in the order they first did.
    pub fn classes_with_const(&self, node: &L) -> Vec<ClassId> {
        if let Some((carrier, value)) = leaf_constant(node) {
            let Some(zero) = self.zero(carrier.key) else {
                return Vec::new();
            };
            let constant = Ref::new(zero, value);
            let mut out = Vec::new();
            match self.db.is_canonical(CLASSES) {
                // A rebuilt union-find is flat: one scan finds the zero's set.
                true => tir_adt::simd::select_eq(self.uf().parents(), zero.0, &mut out),
                false => out.extend(
                    (0..self.class_count() as u32).filter(|&id| self.root(ClassId(id)) == zero),
                ),
            }
            out.retain(|&id| self.find(ClassId(id)) == constant);
            return out.into_iter().map(ClassId).collect();
        }
        let consts = self.db.lattice(CONSTS);
        node.constant()
            .and_then(|constant| self.labels.get(&constant))
            .into_iter()
            .flat_map(|label| consts.keys_with(u64::from(label.0)))
            .map(ClassId)
            .collect()
    }

    /// Whether `reference` is the carrier constant `value`; `None` when
    /// `value` is not an integer constant of a carrier.
    pub(crate) fn is_literal(&self, reference: Ref, value: &L) -> Option<bool> {
        let (carrier, constant) = leaf_constant(value)?;
        let zero = self.zero(carrier.key);
        Some(zero.is_some_and(|zero| self.find(reference) == Ref::new(zero, constant)))
    }

    /// The node interned under `label`.
    pub fn label_node(&self, label: LabelId) -> Option<&L> {
        (label.index() < self.labels.len()).then(|| self.labels.node(label))
    }

    /// `class`'s value in `column`, as one word.
    pub fn fact(&self, column: ColumnId, class: ClassId) -> Option<u64> {
        self.db.lattice(lattice(column)).get(self.root(class).0)
    }

    /// Raise the type `class`'s terms carry, for a term whose type the language
    /// keeps outside the node.
    pub fn raise_type(&mut self, class: ClassId, key: u64) {
        let class = self.root(class);
        self.db.raise(TYPES, class.0, key);
    }

    /// Whether `class`'s value in `column` rose during the round the last
    /// [`Self::take_changed`] closed: the fact-level counterpart of a row in
    /// [`Groups::new_rows`](tir_relational::Groups::new_rows).
    pub fn fact_is_new(&self, column: ColumnId, class: ClassId) -> bool {
        self.db.is_new(lattice(column), class.0)
    }

    // ---- scopes -----------------------------------------------------------

    /// Enter an assumption scope: everything until the matching
    /// [`Self::pop_context`] is undone by it.
    pub fn push_context(&mut self) {
        let members = self.members.last().cloned().unwrap_or_default();
        self.members.push(members);
        self.offsets.push(self.operand_offsets.len());
        self.db.push_scope();
    }

    /// Leave the scope, discarding its unions, its rows, and its assumptions;
    /// the enclosing scope (or the base graph) is restored without a rebuild.
    pub fn pop_context(&mut self) {
        // The classes the scope minted keep their ids, so a caller still holding
        // one finds it, but they lose every e-node the scope gave them and stop
        // being classes: `class_ids` skips a class with none.
        self.db.pop_scope();
        self.members.pop();
        let offsets = self.offsets.pop().expect("open scope");
        let classes = self.class_count();
        let lists = self.db.client_mut();
        // The restored lists link exactly the e-nodes the scope found.
        let nodes = lists.next.len();
        lists.head.resize(classes, NONE);
        lists.tail.resize(classes, NONE);
        lists.size.resize(classes, 0);
        self.node.truncate(nodes);
        self.node_label.truncate(nodes);
        self.node_class.truncate(nodes);
        self.node_offsets.truncate(nodes);
        self.operand_offsets.truncate(offsets);
    }

    /// The classes the open scope found distinct and grouped under `id`, as of
    /// the last rebuild, ascending. Empty when no scope is open or the scope did
    /// not merge `id` with anything. Side tables built against the graph the
    /// scope opened on are keyed by those, so a query made under a scope
    /// aggregates over this.
    pub fn scope_members(&self, id: ClassId) -> &[ClassId] {
        self.members
            .last()
            .and_then(|members| members.get(&id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Canonical classes the open scopes changed: the ones their unions merged,
    /// the ones minted inside them, and transitively every class holding a node
    /// with such a child. Ascending id. Empty with no scope open.
    pub fn scope_dirty(&self) -> Vec<ClassId> {
        self.dirty_since(0)
    }

    /// The same, counted from the innermost scope alone: what changed since the
    /// scope enclosing it, so a caller that already answered for that one has
    /// only this to redo. Ascending id. Empty with no scope open.
    pub fn innermost_dirty(&self) -> Vec<ClassId> {
        self.dirty_since(self.db.depth().saturating_sub(1))
    }

    /// Everything the scopes from `depth` outward-in changed, closed upward.
    fn dirty_since(&self, depth: usize) -> Vec<ClassId> {
        if !self.in_scope() {
            return Vec::new();
        }
        let seeds = self
            .db
            .scope_touched(CLASSES, depth)
            .chain(self.db.lattice(CONSTS).scoped_keys_from(depth));
        class_ids(self.db.close_upward(CLASSES, seeds, None))
    }

    /// `changed` closed upward `height` times over parent edges, ascending: the
    /// classes a pattern of that height can newly match at. A class outside it
    /// has an unchanged downward cone to that depth, so its matches are the
    /// previous round's and were applied then.
    pub fn delta(&self, changed: &[ClassId], height: usize) -> Vec<ClassId> {
        let seeds = changed.iter().map(|id| id.0);
        class_ids(self.db.close_upward(CLASSES, seeds, Some(height)))
    }
}

fn class_ids(ids: Vec<u32>) -> Vec<ClassId> {
    ids.into_iter().map(ClassId).collect()
}

/// The lattice column of `column`.
pub(crate) fn lattice(column: ColumnId) -> LatticeId {
    match column {
        ColumnId::Const => CONSTS,
        ColumnId::Type => TYPES,
    }
}

/// A class, read through the engine. Not a struct the engine owns: an e-class
/// is a set of e-nodes, and this is the cursor into it.
pub struct ClassRef<'a, L: Label> {
    engine: &'a Engine<L>,
    id: ClassId,
}

impl<L: Label> Clone for ClassRef<'_, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Label> Copy for ClassRef<'_, L> {}

impl<'a, L: Label> ClassRef<'a, L> {
    pub fn id(self) -> ClassId {
        self.id
    }

    pub fn rows(self) -> Rows<'a, L> {
        self.engine.rows(self.id)
    }

    pub fn nodes(self) -> impl Iterator<Item = &'a L> + Clone {
        self.engine.nodes(self.id)
    }

    pub fn len(self) -> usize {
        self.engine.class_len(self.id)
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Walks a class's e-nodes along its intrusive list.
pub struct Rows<'a, L: Label> {
    next: &'a [u32],
    cursor: u32,
    label: PhantomData<&'a L>,
}

impl<L: Label> Clone for Rows<'_, L> {
    fn clone(&self) -> Self {
        Self {
            next: self.next,
            cursor: self.cursor,
            label: PhantomData,
        }
    }
}

impl<L: Label> Iterator for Rows<'_, L> {
    type Item = RowId;

    fn next(&mut self) -> Option<RowId> {
        if self.cursor == NONE {
            return None;
        }
        let row = RowId(self.cursor);
        self.cursor = self.next[row.index()];
        Some(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Term;

    fn leaf(eg: &mut Engine<Term>, op: &str, width: u8) -> Ref {
        eg.add(Term::typed(op, width, &[]))
    }

    fn num(eg: &mut Engine<Term>, width: u8, value: u64) -> Ref {
        eg.add(Term::num(width, value))
    }

    fn op(eg: &mut Engine<Term>, op: &str, width: u8, operands: &[Ref]) -> Ref {
        let node = Term::typed(op, width, &vec![ClassId(0); operands.len()]);
        eg.insert(node, operands)
    }

    fn shifted(base: Ref, offset: u64, width: u8) -> Ref {
        Ref::new(
            base.class,
            base.offset.wrapping_add(offset) & tir_relational::unionfind::mask(width),
        )
    }

    #[test]
    fn commutative_operands_keep_the_order_they_were_written_in() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a")).class;
        let b = eg.add(Term::leaf("b")).class;
        let ab = eg.add(Term::comm("add", &[a, b]));
        let ba = eg.add(Term::comm("add", &[b, a]));
        assert_ne!(ab, ba);
        assert_eq!(eg.nodes(ba.class).next().unwrap().children, vec![b, a]);
    }

    #[test]
    fn classes_with_op_reports_each_class_once_in_minting_order() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a")).class;
        let b = eg.add(Term::leaf("b")).class;
        let key = Term::leaf("a").op_key();
        assert_eq!(eg.classes_with_op(key), vec![a]);
        eg.union(a, b).unwrap();
        assert_eq!(eg.classes_with_op(key), vec![eg.root(a)]);
        assert!(eg.classes_with_op(Term::leaf("zzz").op_key()).is_empty());
    }

    /// A scope is the same graph with more merges, so a lookup under it sees
    /// what congruence over those merges proves, and the pop takes it back.
    #[test]
    fn a_scoped_lookup_sees_the_scopes_congruence() {
        let mut eg = Engine::new();
        let b = eg.add(Term::leaf("b")).class;
        let a = eg.add(Term::leaf("a")).class;
        let fa = eg.add(Term::op("f", &[a]));
        eg.rebuild();
        assert_eq!(eg.lookup(&Term::op("f", &[b])), None);
        eg.push_context();
        eg.union(a, b).unwrap();
        eg.rebuild();
        assert_eq!(eg.lookup(&Term::op("f", &[b])), Some(eg.find(fa)));
        eg.pop_context();
        assert_eq!(eg.lookup(&Term::op("f", &[b])), None);
        assert_eq!(eg.lookup(&Term::op("f", &[a])), Some(eg.find(fa)));
    }

    #[test]
    fn offsets_on_an_add_fold_into_one_row() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 32);
        let y = leaf(&mut eg, "y", 32);
        let (three, five, eight) = (
            num(&mut eg, 32, 3),
            num(&mut eg, 32, 5),
            num(&mut eg, 32, 8),
        );
        let x3 = op(&mut eg, "add", 32, &[x, three]);
        let y5 = op(&mut eg, "add", 32, &[y, five]);
        let first = op(&mut eg, "add", 32, &[x3, y5]);
        let xy = op(&mut eg, "add", 32, &[x, y]);
        let second = op(&mut eg, "add", 32, &[xy, eight]);
        assert_eq!(first, second);
        assert_eq!(first, Ref::new(xy.class, 8));
        // x, y, the carrier's zero and the one add.
        assert_eq!(eg.total_size(), 4);
    }

    #[test]
    fn an_identity_element_returns_the_other_operand() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 32);
        let zero = num(&mut eg, 32, 0);
        assert_eq!(op(&mut eg, "add", 32, &[x, zero]), x);
        assert_eq!(op(&mut eg, "add", 32, &[zero, x]), x);
        assert_eq!(op(&mut eg, "sub", 32, &[x, zero]), x);
        // Zero is no identity on the left of a sub.
        assert_ne!(op(&mut eg, "sub", 32, &[zero, x]).class, x.class);
    }

    #[test]
    fn constants_are_offsets_of_one_zero_and_store_no_rows() {
        let mut eg = Engine::new();
        let five = num(&mut eg, 8, 5);
        let seven = num(&mut eg, 8, 7);
        let wide = num(&mut eg, 16, 7);
        assert_eq!(five.class, seven.class);
        assert_ne!(seven.class, wide.class);
        // One zero row per carrier.
        assert_eq!(eg.total_size(), 2);
        assert_eq!(eg.int_const(seven), Some(7));
        assert_eq!(eg.const_of(seven), Some(Term::num(8, 7)));
        // Two constants sum without a row.
        assert_eq!(
            op(&mut eg, "add", 8, &[five, seven]),
            Ref::new(five.class, 12)
        );
        assert_eq!(op(&mut eg, "neg", 8, &[five]), Ref::new(five.class, 251));
        assert_eq!(eg.total_size(), 2);
    }

    #[test]
    fn a_typed_constant_types_its_zero() {
        let mut eg = Engine::new();
        let five = eg.add(Term {
            ty: Some(3),
            ..Term::num(8, 5)
        });
        assert_eq!(eg.fact(ColumnId::Type, five.class), Some(3));
    }

    #[test]
    fn a_biased_linear_node_folds_constants_with_its_bias() {
        let mut eg = Engine::new();
        let five = num(&mut eg, 8, 5);
        // not(5) == -1 - 5 at eight bits.
        assert_eq!(op(&mut eg, "not", 8, &[five]), Ref::new(five.class, 250));
        // not(x + 3) == not(x) - 3, one row.
        let x = leaf(&mut eg, "x", 8);
        let three = num(&mut eg, 8, 3);
        let x3 = op(&mut eg, "add", 8, &[x, three]);
        let not_x = op(&mut eg, "not", 8, &[x]);
        assert_eq!(
            op(&mut eg, "not", 8, &[x3]),
            Ref::new(not_x.class, not_x.offset.wrapping_sub(3) & 0xff)
        );
    }

    #[test]
    fn a_shifted_union_makes_parents_congruent_at_the_shift() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 32);
        let y = leaf(&mut eg, "y", 32);
        let three = num(&mut eg, 32, 3);
        let x3 = op(&mut eg, "add", 32, &[x, three]);
        let g_y = op(&mut eg, "g", 32, &[y]);
        let g_x3 = op(&mut eg, "g", 32, &[x3]);
        let neg_y = op(&mut eg, "neg", 32, &[y]);
        let neg_x = op(&mut eg, "neg", 32, &[x]);
        eg.rebuild();
        assert!(!eg.connected(g_y, g_x3));
        eg.union(y, x3).unwrap();
        eg.rebuild();
        assert!(eg.connected(y, shifted(x, 3, 32)));
        // A parent without a coefficient re-keys at the shift...
        assert!(eg.connected(g_y, g_x3));
        // ...and one with moves the shift to its value: -(x + 3) == -x - 3.
        assert!(eg.connected(neg_y, shifted(neg_x, 3u64.wrapping_neg(), 32)));
    }

    #[test]
    fn an_offset_without_a_coefficient_is_part_of_the_key() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 32);
        let one = num(&mut eg, 32, 1);
        let x1 = op(&mut eg, "add", 32, &[x, one]);
        let g_x = op(&mut eg, "g", 32, &[x]);
        let g_x1 = op(&mut eg, "g", 32, &[x1]);
        assert_ne!(g_x.class, g_x1.class);
        assert_eq!(op(&mut eg, "g", 32, &[x1]), g_x1);
        assert_eq!(
            eg.children(eg.rows(g_x1.class).next().unwrap()).as_slice(),
            &[x1]
        );
    }

    #[test]
    fn offsets_wrap_at_the_carriers_width() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 8);
        let y = leaf(&mut eg, "y", 8);
        let (big, more) = (num(&mut eg, 8, 200), num(&mut eg, 8, 100));
        let partial = op(&mut eg, "add", 8, &[x, big]);
        assert_eq!(
            op(&mut eg, "add", 8, &[partial, more]),
            Ref::new(x.class, 44)
        );
        eg.union(y, Ref::new(x.class, 255)).unwrap();
        eg.rebuild();
        assert_eq!(eg.find(Ref::new(y.class, 1)), x);
    }

    #[test]
    fn a_class_united_with_itself_at_an_offset_is_reported_and_kept() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 8);
        let y = leaf(&mut eg, "y", 8);
        eg.union(y, Ref::new(x.class, 2)).unwrap();
        assert!(!eg.contradicted());
        eg.push_context();
        assert_eq!(eg.union(y, x), Err(Contradiction));
        eg.rebuild();
        assert!(eg.contradicted());
        assert!(eg.conflicted(y.class));
        // The first placement stands.
        assert_eq!(eg.find(y), Ref::new(x.class, 2));
        eg.pop_context();
        assert!(!eg.contradicted());
        assert!(!eg.conflicted(x.class));
    }

    #[test]
    fn a_pop_restores_offsets_and_the_constants_a_scope_assumed() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 32);
        let y = leaf(&mut eg, "y", 32);
        let g_y = op(&mut eg, "g", 32, &[y]);
        eg.rebuild();
        eg.push_context();
        eg.union(y, Ref::new(x.class, 3)).unwrap();
        eg.assume_const(x, Term::num(32, 4));
        eg.rebuild();
        assert_eq!(eg.int_const(y), Some(7));
        assert_eq!(eg.assumed_const(y.class), Some(Term::num(32, 7)));
        assert_eq!(eg.classes_with_const(&Term::num(32, 7)), vec![y.class]);
        eg.pop_context();
        assert_eq!(eg.find(y), y);
        assert_eq!(eg.int_const(y), None);
        assert_eq!(eg.find(g_y), g_y);
        assert_eq!(op(&mut eg, "g", 32, &[y]), g_y);
    }

    /// A row a constant made an identity, or made all constants, leaves its
    /// table and its value joins what it now is.
    #[test]
    fn a_rebuild_dissolves_rows_their_laws_now_settle() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 8);
        let y = leaf(&mut eg, "y", 8);
        let sum = op(&mut eg, "add", 8, &[x, y]);
        let negated = op(&mut eg, "neg", 8, &[y]);
        eg.rebuild();
        let add = Term::typed("add", 8, &[]).op_key();
        assert_eq!(eg.classes_with_op(add).len(), 1);
        eg.push_context();
        eg.assume_const(y, Term::num(8, 5));
        eg.rebuild();
        assert!(eg.connected(sum, Ref::new(x.class, 5)));
        assert_eq!(eg.int_const(negated), Some(251));
        assert!(eg.classes_with_op(add).is_empty());
    }

    /// Assuming a constant re-keys the rows that named the class, not every
    /// row naming a constant of its carrier.
    #[test]
    fn assuming_a_constant_dirties_the_users_of_the_class_alone() {
        let mut eg = Engine::new();
        let x = leaf(&mut eg, "x", 32);
        let g_x = op(&mut eg, "g", 32, &[x]);
        let h_g = op(&mut eg, "h", 32, &[g_x]);
        let mut users = Vec::new();
        for value in 10..60 {
            let constant = num(&mut eg, 32, value);
            let user = op(&mut eg, "g", 32, &[constant]);
            users.push(op(&mut eg, "h", 32, &[user]).class);
        }
        eg.rebuild();
        eg.take_changed();
        eg.push_context();
        eg.assume_const(x, Term::num(32, 5));
        eg.rebuild();
        let dirty = eg.scope_dirty();
        assert!(dirty.contains(&eg.root(g_x.class)) && dirty.contains(&eg.root(h_g.class)));
        assert!(dirty.len() <= 4, "{dirty:?}");
        let changed = eg.take_changed().expect("a narrowed log");
        let frontier = eg.delta(&changed, 2);
        assert!(users.iter().all(|user| !frontier.contains(user)));
        assert!(frontier.len() <= 4, "{frontier:?}");
    }
}
