use std::cell::RefCell;
use std::marker::PhantomData;

use smallvec::SmallVec;

use tir_relational::query::Resolution;
use tir_relational::saturate::Caches;
use tir_relational::store::{Repair, Table};
use tir_relational::{Database, Join, LatticeId, SpaceId, Stats};

use crate::label::Labels;
use crate::{ClassId, ColumnId, FxHashMap, Label, LabelId, RowId};

/// Empty link in an intrusive list.
const NONE: u32 = u32::MAX;

/// The id space classes are minted in, the database's only one.
pub(crate) const CLASSES: SpaceId = SpaceId::nth(0);

/// The constant a class is known to be, as a label: seeded by every literal
/// row, raised by a scope's assumption, joined by a union.
///
/// The lattice columns are declared in the order a search from a risen fact
/// tries them, which is how rarely they rise: a constant, then a derivation,
/// then a type, which nearly every new class is minted with.
pub(crate) const CONSTS: LatticeId = LatticeId::nth(0);
/// The class a class is derived from and the distance to it: pointer
/// provenance, for the vocabulary that has any. Two derivations that disagree
/// conflict, which reads back as "derived from nothing known", the
/// conservative answer.
pub(crate) const OBJECTS: LatticeId = LatticeId::nth(1);
/// The type a class's terms carry, seeded by every typed row. Congruence
/// already forces a class's rows to agree on it, so the first row to say wins
/// and a merge does not make the answer depend on merge order.
pub(crate) const TYPES: LatticeId = LatticeId::nth(2);

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

    /// Per open scope, the classes it found distinct, grouped under the class
    /// it merged them into; merged groups only. Starts as the enclosing
    /// scope's, so a nested scope keeps naming base classes.
    members: Vec<FxHashMap<ClassId, Vec<ClassId>>>,

    repair: Repair,
    /// Per plan, what its searches keep, and which rules a round visits for
    /// the rule set last saturated with. Labels only accumulate, so an entry
    /// stays valid for the engine's life.
    pub(crate) caches: RefCell<Caches>,
    /// Scratch for the variables a rule head binds.
    pub(crate) head_bound: Vec<Option<u32>>,
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
            db.add_offsets(CLASSES),
            db.add_lattice(CLASSES, Join::First),
        ];
        debug_assert_eq!(facts, [CONSTS, OBJECTS, TYPES]);
        Self {
            db,
            labels: Labels::default(),
            node: Vec::new(),
            node_label: Vec::new(),
            op_labels: FxHashMap::default(),
            label_slot: Vec::new(),
            label_commutes: Vec::new(),
            members: Vec::new(),
            repair: Repair::default(),
            caches: RefCell::default(),
            head_bound: Vec::new(),
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
        self.db.stats()
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
            if !matches || commuting && !self.label_commutes[label.index()] {
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

    pub(crate) fn make_class(&mut self, node: L, label: LabelId, key: &[u32]) -> ClassId {
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
        if tir_relational::trace_enabled() {
            eprintln!("A {} {:?} {:?}", class, node, &key[1..]);
        }
        self.node.push(node);
        self.node_label.push(label);
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
        self.db
            .raise(CONSTS, self.find(class).0, u64::from(label.0));
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
        self.db.raise_offset(OBJECTS, class.0, base.0, offset)
    }

    /// What `class` is derived from: the end of its derivation chain, or itself
    /// at offset zero when nothing derived it. `None` once two derivations have
    /// disagreed, and for a chain that does not end — a class derived from
    /// itself is a contradiction, and "nowhere known" is the conservative
    /// reading of one.
    pub fn object_of(&self, class: ClassId) -> Option<(ClassId, i64)> {
        let (base, offset) = self.db.offset(OBJECTS, class.0)?;
        Some((ClassId(base), offset))
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
pub(crate) fn lattice(column: ColumnId) -> LatticeId {
    match column {
        ColumnId::Const => CONSTS,
        ColumnId::Object => OBJECTS,
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
