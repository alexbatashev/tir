use std::cell::RefCell;
use std::marker::PhantomData;

use smallvec::SmallVec;

use crate::column::{Fact, Join};
use crate::db::{Database, LatticeId, SpaceId};
use crate::label::{FxHashMap, Labels};
use crate::saturate::{RuleCache, RuleIndex};
use crate::store::{Repair, Table};
use crate::{ClassId, ColumnId, Label, LabelId, RowId};

/// Empty link in an intrusive list.
const NONE: u32 = u32::MAX;

/// The id space classes are minted in, the database's only one.
pub(crate) const CLASSES: SpaceId = SpaceId::nth(0);

/// The constant a class is known to be, as a label: seeded by every literal
/// row, raised by a scope's assumption, joined by a union. The lattice columns
/// are declared in [`column_bit`] order, so a column's bit is its id.
const CONSTS: LatticeId = LatticeId::nth(0);
/// The type a class's terms carry, seeded by every typed row. Congruence
/// already forces a class's rows to agree on it, so the first row to say wins
/// and a merge does not make the answer depend on merge order.
const TYPES: LatticeId = LatticeId::nth(1);
/// The class a class is derived from and the distance to it, as an index into
/// [`Engine::derivations`]: pointer provenance, for the vocabulary that has
/// any. A fact naming a class is canonicalized like a child, and two
/// derivations that disagree conflict — which reads back as "derived from
/// nothing known", the conservative answer.
const OBJECTS: LatticeId = LatticeId::nth(2);

/// A class read as a distance from another class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Object {
    base: ClassId,
    offset: i64,
}

/// The e-graph: equivalence classes of terms, as a client of a [`Database`].
///
/// Classes are the ids of one space of the database. Each label has a table
/// keyed on the child classes and valued by the class the node belongs to, so
/// hash-consing is a key lookup and congruence is the table's functional
/// dependency: a rebuild canonicalizes the tables and merges the classes of the
/// rows that come to share a key. A rule's atoms are joins over those tables.
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

    /// The term each e-node was interned as, and its label.
    node: Vec<L>,
    node_label: Vec<LabelId>,
    /// [`Label::op_key`] -> the labels under it, in the order they were first
    /// seen.
    op_labels: FxHashMap<u64, Vec<LabelId>>,
    /// Per label, the table its rows live in and whether it is a commutative
    /// binary operator.
    label_slot: Vec<u32>,
    label_commutes: Vec<bool>,
    label_op: Vec<u64>,

    /// Every derivation [`OBJECTS`] has held. A lattice value is one word, so the
    /// column holds the index; only ever appended to, like the labels, so an
    /// index a popped scope wrote never needs taking back.
    derivations: Vec<Object>,
    derivation_ids: FxHashMap<Object, u64>,
    /// Per open scope, the classes it found distinct, grouped under the class
    /// it merged them into; merged groups only. Starts as the enclosing
    /// scope's, so a nested scope keeps naming base classes.
    members: Vec<FxHashMap<ClassId, Vec<ClassId>>>,

    repair: Repair,
    /// Per plan, what its searches keep: the labels its atoms read and the
    /// labels its head builds. Labels only accumulate, so an entry stays
    /// valid for the engine's life.
    caches: RefCell<FxHashMap<u64, RuleCache>>,
    /// Which rules a round visits, for the rule set last saturated with.
    pub(crate) rule_index: Option<std::sync::Arc<RuleIndex>>,
    /// Scratch for the variables a rule head binds.
    pub(crate) head_bound: Vec<Option<ClassId>>,
    stats: Stats,
}

/// The e-nodes of each class as an intrusive list, kept in the database so a
/// scope undoes it.
#[derive(Clone, Default)]
pub(crate) struct Lists {
    /// Per e-node, the next e-node of its class, or [`NONE`].
    next: Vec<u32>,
    /// Per class, its first and last e-node and how many it has.
    head: Vec<u32>,
    tail: Vec<u32>,
    size: Vec<u32>,
}

/// The labels an atom reads, by the table they live in; ascending within one.
pub(crate) type Candidates = SmallVec<[(u32, SmallVec<[LabelId; 4]>); 2]>;

/// Cumulative engine work, for the saturation counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub merges: usize,
    pub adds: usize,
    /// Rows a rebuild re-keyed.
    pub repairs: usize,
    /// Column entries that rose. A round that only raised a fact changed
    /// nothing the class and node counts see, and is not a fixpoint.
    pub raises: usize,
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
            db.add_lattice(CLASSES, Join::Agree),
        ];
        debug_assert_eq!(facts, [CONSTS, TYPES, OBJECTS]);
        Self {
            db,
            labels: Labels::default(),
            node: Vec::new(),
            node_label: Vec::new(),
            op_labels: FxHashMap::default(),
            label_slot: Vec::new(),
            label_commutes: Vec::new(),
            label_op: Vec::new(),
            derivations: Vec::new(),
            derivation_ids: FxHashMap::default(),
            members: Vec::new(),
            repair: Repair::default(),
            caches: RefCell::default(),
            rule_index: None,
            head_bound: Vec::new(),
            stats: Stats::default(),
        }
    }

    // ---- reading ----------------------------------------------------------

    pub fn find(&self, id: ClassId) -> ClassId {
        ClassId(self.db.find(CLASSES, id.0))
    }

    pub fn connected(&self, a: ClassId, b: ClassId) -> bool {
        self.find(a) == self.find(b)
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
        self.node.len() * (size_of::<L>() + 2 * size_of::<u32>())
            + cells * size_of::<u32>()
            + self.class_count() * 5 * size_of::<u32>()
            + self.labels.len() * size_of::<L>()
    }

    /// Work done since the engine was built. A saturation round reads the
    /// difference across it; a scope does not roll these back, since they count
    /// work, not state.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    fn in_scope(&self) -> bool {
        self.db.depth() != 0
    }

    /// Child classes of an e-node, canonical as of now.
    pub fn children(&self, row: RowId) -> SmallVec<[ClassId; 4]> {
        self.node[row.index()]
            .children()
            .iter()
            .map(|&child| self.find(child))
            .collect()
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
            cursor: lists.head[self.find(id).index()],
            label: PhantomData,
        }
    }

    /// E-nodes of `id`'s class; child ids may be non-canonical — resolve with
    /// [`Self::find`].
    pub fn nodes(&self, id: ClassId) -> impl Iterator<Item = &L> + Clone {
        self.rows(id).map(|row| &self.node[row.index()])
    }

    fn class_len(&self, id: ClassId) -> usize {
        self.db.client().size[self.find(id).index()] as usize
    }

    /// Every live class, ascending. A class is named by its lowest member, so
    /// under a scope this is the position of the group's lowest base class.
    /// Extraction and bare-variable-rooted patterns walk classes in this order
    /// and break ties by it, so it is part of the contract.
    pub fn class_ids(&self) -> impl Iterator<Item = ClassId> + '_ {
        let size = &self.db.client().size;
        (0..self.class_count() as u32)
            .map(ClassId)
            .filter(move |&id| self.find(id) == id && size[id.index()] != 0)
    }

    /// [`Self::class_ids`] as readable classes.
    pub fn classes(&self) -> impl Iterator<Item = ClassRef<'_, L>> + '_ {
        self.class_ids().map(|id| self.class(id))
    }

    pub fn class(&self, id: ClassId) -> ClassRef<'_, L> {
        ClassRef {
            engine: self,
            id: self.find(id),
        }
    }

    /// Whether `label` is a commutative binary operator.
    pub(crate) fn commutes(&self, label: LabelId) -> bool {
        self.label_commutes[label.index()]
    }

    /// The labels whose operator bucket is `op`, in the order first seen.
    pub(crate) fn labels_with_op(&self, op: u64) -> &[LabelId] {
        self.op_labels.get(&op).map_or(&[], Vec::as_slice)
    }
    /// Add to `reads` the labels in the `op` bucket that `template` matches at
    /// its arity, or every label in the bucket without one. With `since`, only
    /// the labels met from that count on are looked at, and `reads` already
    /// holds the rest.
    pub(crate) fn labels_matching(
        &self,
        op: u64,
        template: Option<(&L, usize)>,
        since: Option<usize>,
        reads: &mut Candidates,
    ) {
        let matches = |label: LabelId| {
            let node = self.labels.node(label);
            template.is_none_or(|(template, arity)| {
                node.children().len() == arity && template.matches_template(node)
            })
        };
        let mut add = |label: LabelId| {
            let slot = self.label_slot[label.index()];
            // Labels arrive ascending, so each table's list stays sorted.
            match reads.iter_mut().find(|(known, _)| *known == slot) {
                Some((_, labels)) => labels.push(label),
                None => reads.push((slot, SmallVec::from_slice(&[label]))),
            }
        };
        match since {
            None => {
                for &label in self.labels_with_op(op) {
                    if matches(label) {
                        add(label);
                    }
                }
            }
            Some(since) => {
                for index in since..self.label_op.len() {
                    let label = LabelId(index as u32);
                    if self.label_op[index] == op && matches(label) {
                        add(label);
                    }
                }
            }
        }
    }

    /// How many labels the graph has met. What a plan's atoms read is fixed
    /// between two readings that agree.
    pub(crate) fn labels_len(&self) -> usize {
        self.labels.len()
    }

    /// The columns, as a mask of [`column_bit`], in which a fact rose during
    /// the round the last [`Self::take_changed`] closed: on a class that
    /// already existed, or with `minted` on a class minted with it.
    pub(crate) fn facts_rose(&self, minted: bool) -> u8 {
        [ColumnId::Const, ColumnId::Type, ColumnId::Object]
            .into_iter()
            .filter(|&column| !self.db.risen(lattice(column), minted).is_empty())
            .fold(0, |mask, column| mask | 1 << column_bit(column))
    }

    /// The classes whose fact in `column` rose during the round the last
    /// [`Self::take_changed`] closed; `minted` as for [`Self::facts_rose`].
    pub(crate) fn risen(&self, column: ColumnId, minted: bool) -> impl Iterator<Item = ClassId> {
        let risen = self.db.risen(lattice(column), minted);
        risen.iter().map(|&class| ClassId(class))
    }

    /// Take the cache of plan `id` out for a search; [`Self::put_cache`] puts
    /// it back. Plan zero has none.
    pub(crate) fn take_cache(&self, id: u64) -> RuleCache {
        self.caches.borrow_mut().remove(&id).unwrap_or_default()
    }

    pub(crate) fn put_cache(&self, id: u64, cache: RuleCache) {
        if id != 0 {
            self.caches.borrow_mut().insert(id, cache);
        }
    }

    /// Every plan's cache, for a saturation to use and hand back.
    pub(crate) fn take_caches(&mut self) -> FxHashMap<u64, RuleCache> {
        std::mem::take(self.caches.get_mut())
    }

    pub(crate) fn put_caches(&mut self, caches: FxHashMap<u64, RuleCache>) {
        *self.caches.get_mut() = caches;
    }

    /// The operator buckets of the rows the previous round wrote, each once.
    pub(crate) fn new_ops(&self) -> Vec<u64> {
        let slots = self.db.tables().len() as u32;
        for slot in 0..slots {
            self.db.prepare_new_rows(slot);
        }
        let groups = self.db.groups();
        let mut ops = Vec::new();
        for slot in 0..slots {
            let mut last = None;
            // The rows are sorted by label, so each label shows up as a run.
            for &(label, _) in groups.new_rows(slot) {
                if last.replace(label) != Some(label) {
                    ops.push(self.label_op[label as usize]);
                }
            }
        }
        ops.sort_unstable();
        ops.dedup();
        ops
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
                let root = self.find(ClassId(table.values()[row as usize]));
                if seen.insert(root.index()) {
                    out.push(root);
                }
            }
        }
        out
    }

    /// Class of an already-interned `node`, or `None` (never inserts; always
    /// `None` for a unique node).
    pub fn lookup(&self, node: &L) -> Option<ClassId> {
        let label = self.labels.get(node)?;
        let mut key: SmallVec<[u32; 8]> = SmallVec::from_slice(&[label.0]);
        key.extend(node.children().iter().map(|&child| self.find(child).0));
        self.memo_find(label, &key)
    }

    /// The class of the row with `key`, which is `label` and then the canonical
    /// children.
    fn memo_find(&self, label: LabelId, key: &[u32]) -> Option<ClassId> {
        let table = self.db.table(*self.label_slot.get(label.index())?)?;
        let row = table.get(key)?;
        Some(self.find(ClassId(table.values()[row as usize])))
    }

    // ---- writing ----------------------------------------------------------

    /// Intern `node`, returning its class. A non-unique node equal to an
    /// existing one shares its class; otherwise a fresh class.
    pub fn add(&mut self, mut node: L) -> ClassId {
        for child in node.children_mut() {
            *child = self.find(*child);
        }
        let label = self.intern(&node);
        let mut key: SmallVec<[u32; 8]> = SmallVec::new();
        key.push(label.0);
        key.extend(node.children().iter().map(|child| child.0));
        self.add_labelled(label, &key, || node)
    }
    /// Whether `label` is `node`'s label.
    pub(crate) fn label_is(&self, label: LabelId, node: &L) -> bool {
        let known = self.labels.node(label);
        known.children().len() == node.children().len() && known.matches(node)
    }

    /// The id of `node`'s label, for [`Self::add_labelled`].
    pub(crate) fn intern(&mut self, node: &L) -> LabelId {
        let label = self.labels.intern(node);
        // Whatever the label table grew by gets its table and operator bucket.
        for index in self.label_slot.len()..self.labels.len() {
            let node = self.labels.node(LabelId(index as u32));
            let arity = node.children().len();
            self.label_slot
                .push(2 * arity as u32 + u32::from(node.is_unique()));
            self.label_commutes.push(arity == 2 && node.commutative());
            let op = node.op_key();
            self.label_op.push(op);
            self.op_labels
                .entry(op)
                .or_default()
                .push(LabelId(index as u32));
        }
        label
    }

    /// [`Self::add`] for a caller that already has the key, which is the label
    /// and then the canonical children: `node` is built only if the graph does
    /// not hold the node yet.
    pub(crate) fn add_labelled(
        &mut self,
        label: LabelId,
        key: &[u32],
        node: impl FnOnce() -> L,
    ) -> ClassId {
        match self.memo_find(label, key) {
            Some(class) => class,
            None => self.make_class(node(), label, key),
        }
    }

    /// Merge the classes of `a` and `b`, returning the survivor. Congruence
    /// repair is deferred to [`Self::rebuild`], and so is everything a query
    /// reads: the tables keep naming the absorbed class until then, so a search
    /// needs a rebuild first. The merge
    /// itself is visible immediately to [`Self::find`] and [`Self::nodes`], so
    /// an applier that unions and then instantiates hash-conses against the
    /// result.
    pub fn union(&mut self, a: ClassId, b: ClassId) -> ClassId {
        let (survivor, absorbed) = match self.db.equate(CLASSES, a.0, b.0) {
            (root, None) => return ClassId(root),
            (survivor, Some(absorbed)) => (survivor, absorbed),
        };
        self.stats.merges += 1;
        self.db.merge(CONSTS, absorbed, survivor);
        self.db.merge(TYPES, absorbed, survivor);
        if !self.db.lattice(OBJECTS).is_empty() {
            self.merge_object(ClassId(absorbed), ClassId(survivor));
        }
        self.splice_class(survivor as usize, absorbed as usize);
        let (survivor, absorbed) = (ClassId(survivor), ClassId(absorbed));
        if let Some(members) = self.members.last_mut() {
            let taken = members.remove(&absorbed).unwrap_or_else(|| vec![absorbed]);
            members
                .entry(survivor)
                .or_insert_with(|| vec![survivor])
                .extend(taken);
        }
        survivor
    }

    /// Restore congruence to a fixpoint after a batch of unions.
    ///
    /// Each pass canonicalizes every table and merges the classes of the rows
    /// that came to share a key; those merges are what the next pass rewrites.
    /// Nothing depends on the order the tables are visited in or the rows
    /// within one: the class a merge leaves standing is the smallest id in the
    /// set whichever way the merges are grouped.
    pub fn rebuild(&mut self) {
        let mut report = std::mem::take(&mut self.repair);
        while self.db.canonicalize(CLASSES, &mut report) {
            self.stats.repairs += report.rekeyed.len();
            for collision in report.collisions.drain(..) {
                self.union(ClassId(collision.kept), ClassId(collision.removed));
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

    fn make_class(&mut self, node: L, label: LabelId, key: &[u32]) -> ClassId {
        let constant = node.constant();
        let type_key = node.type_key();
        let row = self.node.len() as u32;
        let class = self.db.mint(CLASSES);
        let slot = self.label_slot[label.index()];
        // The table a label's rows live in is made with every table below it
        // when it gets its first row. Labels of one arity share a keyed table,
        // and beside it a bag for the labels that never hash-cons.
        while self.db.tables().len() <= slot as usize {
            let at = self.db.tables().len();
            let columns = 1 + at / 2;
            let table = match at % 2 {
                1 => Table::bag(columns),
                _ => Table::new(columns),
            };
            // The label is a key column the union-find has no say over.
            self.db.add_table(table.plain(1), CLASSES);
        }
        self.db.insert(slot, key, class, row);
        let lists = self.db.client_mut();
        lists.next.push(NONE);
        lists.head.push(row);
        lists.tail.push(row);
        lists.size.push(1);
        if crate::trace_enabled() {
            eprintln!("A {} {:?} {:?}", class, node, &key[1..]);
        }
        self.node.push(node);
        self.node_label.push(label);
        if let Some(constant) = constant {
            let label = self.intern(&constant);
            if self.db.seed(CONSTS, class, u64::from(label.0)) {
                self.stats.raises += 1;
            }
        }
        if let Some(key) = type_key {
            self.db.seed(TYPES, class, key);
        }
        self.stats.adds += 1;
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

    /// Raise, inside the current scope, that `class` evaluates to constant
    /// `node`. A fact, not a merge: the class keeps its identity and parents, so
    /// only its users see a change. Panics with no scope open — an unscoped
    /// assumption would never be popped.
    pub fn assume_const(&mut self, class: ClassId, node: L) {
        assert!(
            self.in_scope(),
            "an assumption needs a scope to be undone by"
        );
        let label = self.intern(&node);
        self.raise_const(self.find(class), label);
    }

    /// The constant `class` is known to be — its own literal row, or what an
    /// open scope assumed of it. `None` when nothing is known and when two
    /// values were proven, which a refuted hypothesis reads as "unknown".
    pub fn const_of(&self, class: ClassId) -> Option<&L> {
        self.db
            .lattice(CONSTS)
            .get(self.find(class).0)
            .map(|label| self.labels.node(LabelId(label as u32)))
    }

    /// The constant an *open scope* assumed of `class`, as opposed to the one
    /// the class states about itself. A reader that acts on a hypothesis rather
    /// than on the program asks this.
    pub fn assumed_const(&self, class: ClassId) -> Option<&L> {
        let class = self.find(class);
        self.db
            .lattice(CONSTS)
            .written_in_scope(class.0)
            .then(|| self.const_of(class))
            .flatten()
    }

    /// The class that holds `node` as its own literal, whatever type the row
    /// carries. The hash-cons cannot answer this: a class is known to be a
    /// *value*, and the row saying so may spell it at a type the caller has no
    /// way to name.
    pub fn const_class(&self, node: &L) -> Option<ClassId> {
        let consts = self.db.lattice(CONSTS);
        self.classes_with_const(node)
            .find(|&class| !consts.written_in_scope(class.0))
    }

    /// The classes an open scope assumed to be `node`.
    pub fn classes_assumed_const<'a>(&'a self, node: &L) -> impl Iterator<Item = ClassId> + 'a {
        let consts = self.db.lattice(CONSTS);
        self.classes_with_const(node)
            .filter(|&class| consts.written_in_scope(class.0))
    }

    /// Whether `class` was proven two different constants — a refuted scope.
    pub fn const_conflicted(&self, class: ClassId) -> bool {
        self.db.lattice(CONSTS).is_conflicted(self.find(class).0)
    }

    /// The classes known to be `node`.
    pub fn classes_with_const<'a>(&'a self, node: &L) -> impl Iterator<Item = ClassId> + 'a {
        let consts = self.db.lattice(CONSTS);
        node.constant()
            .and_then(|constant| self.labels.get(&constant))
            .into_iter()
            .flat_map(|label| consts.keys_with(u64::from(label.0)))
            .map(ClassId)
    }

    /// The node interned under `label`.
    pub fn label_node(&self, label: LabelId) -> Option<&L> {
        (label.index() < self.labels.len()).then(|| self.labels.node(label))
    }

    /// Raise "`class` is `base` plus `offset`". The value is resolved to the end
    /// of its chain first, so a rule that learns the same location by two routes
    /// — `q` from `p + 4`, then `q` from `(p + 4) + 0` — states one fact rather
    /// than two that disagree. Reports whether the column moved.
    pub fn raise_object(&mut self, class: ClassId, base: ClassId, offset: i64) -> bool {
        let class = self.find(class);
        let raised = match self.resolve(base, offset) {
            // A class derived from itself at no distance is not derived at all;
            // at a distance it is a contradiction.
            Some((base, 0)) if base == class => return false,
            Some((base, offset)) => Fact::Known(Object { base, offset }),
            None => Fact::Conflict,
        };
        // Both sides are read back to where they land before they are compared.
        // What is stored may name a class since absorbed, or a base whose own
        // derivation deepened after it was written, and neither is a second
        // opinion — it is the same one, spelled at the time it was learned.
        let stored = self.resolved_object(class);
        let joined = match stored {
            Some(stored) => stored.join(raised, Join::Agree),
            None => raised,
        };
        if stored == Some(joined) {
            return false;
        }
        let joined = match joined {
            Fact::Known(object) => Fact::Known(self.derivation_id(object)),
            Fact::Conflict => Fact::Conflict,
        };
        self.db.put(OBJECTS, class.0, joined);
        self.stats.raises += 1;
        true
    }

    /// The index `object` is stored under in the `objects` column.
    fn derivation_id(&mut self, object: Object) -> u64 {
        *self.derivation_ids.entry(object).or_insert_with(|| {
            self.derivations.push(object);
            self.derivations.len() as u64 - 1
        })
    }
    /// What `class` is derived from: the end of its derivation chain, or itself
    /// at offset zero when nothing derived it. `None` once two derivations have
    /// disagreed, and for a chain that does not end — a class derived from
    /// itself is a contradiction, and "nowhere known" is the conservative
    /// reading of one.
    pub fn object_of(&self, class: ClassId) -> Option<(ClassId, i64)> {
        self.resolve(class, 0)
    }

    /// Follow the chain from `base` to the class nothing derives, adding the
    /// distances. Bounded: the chain is flattened as it is written, so anything
    /// this long is a cycle.
    fn resolve(&self, base: ClassId, offset: i64) -> Option<(ClassId, i64)> {
        const CHAIN_LIMIT: usize = 64;
        let mut base = self.find(base);
        let mut offset = offset;
        for _ in 0..CHAIN_LIMIT {
            match self.canonical_object(base) {
                // Nothing derives it: the end of the chain.
                None => return Some((base, offset)),
                // A class that derives itself ends the chain when it adds
                // nothing, and contradicts itself when it adds something.
                Some(Fact::Known(object)) if object.base == base => {
                    return (object.offset == 0).then_some((base, offset));
                }
                Some(Fact::Known(object)) => {
                    base = object.base;
                    offset = offset.checked_add(object.offset)?;
                }
                Some(Fact::Conflict) => return None,
            }
        }
        None
    }

    /// The stored entry with its base read through the union-find, or `None`
    /// when nothing derived the class. A class the entry names may have been
    /// absorbed since it was written.
    fn canonical_object(&self, class: ClassId) -> Option<Fact<Object>> {
        Some(match self.db.lattice(OBJECTS).entry(class.0)? {
            Fact::Known(id) => {
                let object = self.derivations[id as usize];
                Fact::Known(Object {
                    base: self.find(object.base),
                    offset: object.offset,
                })
            }
            Fact::Conflict => Fact::Conflict,
        })
    }

    /// The stored entry read all the way back to where it lands.
    fn resolved_object(&self, class: ClassId) -> Option<Fact<Object>> {
        Some(match self.canonical_object(class)? {
            Fact::Known(object) => match self.resolve(object.base, object.offset) {
                Some((base, offset)) => Fact::Known(Object { base, offset }),
                None => Fact::Conflict,
            },
            Fact::Conflict => Fact::Conflict,
        })
    }

    fn merge_object(&mut self, absorbed: ClassId, survivor: ClassId) {
        match self.db.detach(OBJECTS, absorbed.0) {
            Some(Fact::Known(id)) => {
                let object = self.derivations[id as usize];
                self.raise_object(survivor, object.base, object.offset);
            }
            Some(Fact::Conflict) => {
                self.db.put(OBJECTS, survivor.0, Fact::Conflict);
            }
            None => {}
        }
    }

    /// `class`'s value in `column`, as one word.
    pub fn fact(&self, column: ColumnId, class: ClassId) -> Option<u64> {
        match column {
            ColumnId::Const | ColumnId::Type => {
                self.db.lattice(lattice(column)).get(self.find(class).0)
            }
            // Not a word: a derivation is a class and a distance, which
            // [`crate::Atom::Object`] binds as a variable and a scalar.
            ColumnId::Object => None,
        }
    }

    /// Raise the type `class`'s terms carry, for a term whose type the language
    /// keeps outside the node.
    pub fn raise_type(&mut self, class: ClassId, key: u64) {
        let class = self.find(class);
        if self.db.raise(TYPES, class.0, key) {
            self.stats.raises += 1;
        }
    }

    /// Whether `class`'s value in `column` rose during the round the last
    /// [`Self::take_changed`] closed — the fact-level [`Self::row_is_new`].
    pub fn fact_is_new(&self, column: ColumnId, class: ClassId) -> bool {
        self.db
            .lattice(lattice(column))
            .is_new(self.find(class).0, self.db.epoch())
    }

    fn raise_const(&mut self, class: ClassId, label: LabelId) {
        if self.db.raise(CONSTS, class.0, u64::from(label.0)) {
            self.stats.raises += 1;
        }
    }

    // ---- scopes -----------------------------------------------------------

    /// Enter an assumption scope: everything until the matching
    /// [`Self::pop_context`] is undone by it.
    pub fn push_context(&mut self) {
        let members = self.members.last().cloned().unwrap_or_default();
        self.members.push(members);
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
        let classes = self.class_count();
        let lists = self.db.client_mut();
        // The restored lists link exactly the e-nodes the scope found.
        self.node.truncate(lists.next.len());
        self.node_label.truncate(lists.next.len());
        lists.head.resize(classes, NONE);
        lists.tail.resize(classes, NONE);
        lists.size.resize(classes, 0);
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
fn lattice(column: ColumnId) -> LatticeId {
    LatticeId::nth(column_bit(column) as u32)
}

/// The position of `column` in a mask of columns.
pub(crate) fn column_bit(column: ColumnId) -> usize {
    match column {
        ColumnId::Const => 0,
        ColumnId::Type => 1,
        ColumnId::Object => 2,
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

    #[test]
    fn commutative_operands_keep_the_order_they_were_written_in() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let ab = eg.add(Term::comm("add", &[a, b]));
        let ba = eg.add(Term::comm("add", &[b, a]));
        assert_ne!(ab, ba);
        assert_eq!(eg.nodes(ba).next().unwrap().children, vec![b, a]);
    }

    #[test]
    fn classes_with_op_reports_each_class_once_in_minting_order() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let key = Term::leaf("a").op_key();
        assert_eq!(eg.classes_with_op(key), vec![a]);
        eg.union(a, b);
        assert_eq!(eg.classes_with_op(key), vec![eg.find(a)]);
        assert!(eg.classes_with_op(Term::leaf("zzz").op_key()).is_empty());
    }

    /// A scope is the same graph with more merges, so a lookup under it sees
    /// what congruence over those merges proves, and the pop takes it back.
    #[test]
    fn a_scoped_lookup_sees_the_scopes_congruence() {
        let mut eg = Engine::new();
        let b = eg.add(Term::leaf("b"));
        let a = eg.add(Term::leaf("a"));
        let fa = eg.add(Term::op("f", &[a]));
        eg.rebuild();
        assert_eq!(eg.lookup(&Term::op("f", &[b])), None);
        eg.push_context();
        eg.union(a, b);
        eg.rebuild();
        assert_eq!(eg.lookup(&Term::op("f", &[b])), Some(eg.find(fa)));
        eg.pop_context();
        assert_eq!(eg.lookup(&Term::op("f", &[b])), None);
        assert_eq!(eg.lookup(&Term::op("f", &[a])), Some(eg.find(fa)));
    }
}
