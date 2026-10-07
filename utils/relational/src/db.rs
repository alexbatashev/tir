//! The database: the tables a client defines, the id spaces their cells name,
//! and everything a fixpoint evaluation and an assumption scope need on top.
//!
//! An id space mints `u32` cells and keeps a union-find over them, which
//! places each id at a constant offset from its representative: a cell and its
//! weight are a [`Ref`]. Equating two references is immediate for
//! [`Database::find`]; the tables catch up when the client asks for
//! [`Database::canonicalize`], which rewrites every table of the space through
//! the union-find, carrying the offsets, and reports the rows that came to
//! share a key. What the client does with those is its own business.
//!
//! Every row written is stamped with the current epoch, and every id minted,
//! equated, re-keyed or raised in a lattice column is logged against its space,
//! so a semi-naive client asks only what the epoch before wrote. A scope is a
//! snapshot of all of it, and of whatever state of its own the client keeps in
//! the database, put back when the scope pops.
//!
//! The indexes a search reads, rows grouped by a column and tries over a
//! column order, keep a base built over the rows a table had and place the
//! rows appended or moved since into it; the base is rebuilt only when those
//! outgrow a fraction of it.

use std::cell::{self, RefCell, RefMut};

use crate::column::{Column, Fact, Join};
use crate::query::trie::Trie;
use crate::store::{Group, Repair, RowView, Shift, Table};
use crate::unionfind::{Merge, UnionFind};
use crate::{Csr, Ref};

/// An id space: the ids one union-find partitions. A database has a fixed
/// number of them, numbered from zero, so a client names them as constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpaceId(u32);

impl SpaceId {
    /// The `index`th space.
    pub const fn nth(index: u32) -> Self {
        Self(index)
    }
}

/// A lattice column over the ids of one space. Lattice columns are numbered
/// from zero in the order they are declared, so a client that declares its
/// schema up front can name them as constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LatticeId(u32);

impl LatticeId {
    /// The `index`th lattice column declared.
    pub const fn nth(index: u32) -> Self {
        Self(index)
    }

    /// The position the column was declared at; also its bit in a mask of
    /// columns.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Tables, id spaces and lattice columns, with the change log and the
/// assumption scopes over all of them. `C` is the client's own state that
/// scopes undo, kept here so a scope snapshots it with the rest. The number of
/// id spaces, `SPACES`, is part of the type, so a constant [`SpaceId`] costs no
/// more than a field.
pub struct Database<C, const SPACES: usize> {
    state: State<C, SPACES>,
    /// Lattice columns keep an undo log per scope rather than a snapshot.
    lattices: Vec<Lattice>,
    scopes: Vec<Scope<C, SPACES>>,
    /// Bumped whenever a table or a partition changes, so a derived index
    /// knows it is stale.
    version: u64,
    groups: RefCell<Groups>,
    /// Per space, the reverse index and the version it was built at.
    reverse: RefCell<Vec<(u64, Csr)>>,
    /// Reusable per-id "already seen" marks for the read-side sweeps, so a
    /// sweep that visits a handful of ids does not first zero an array the size
    /// of the space.
    marks: RefCell<Marks>,
    stats: Stats,
}

/// Cumulative work, for a fixpoint test and the saturation counters. A scope
/// does not roll these back, since they count work, not state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Ids equated away.
    pub merges: usize,
    /// Ids minted.
    pub adds: usize,
    /// Rows a canonicalization re-keyed.
    pub repairs: usize,
    /// Lattice entries a raise moved. A round that only raised a fact changed
    /// no table and no partition, and is not a fixpoint.
    pub raises: usize,
}

/// A row [`Database::canonicalize`] re-keyed, offered to the client with the
/// space's union-find and the client's own state: answering true takes the
/// row out of its table, the client having taken its equation elsewhere.
pub type Dissolve<'a, C> = &'a mut dyn FnMut(&UnionFind, &C, RowView<'_>) -> bool;

/// A reference as the trace spells it: the id alone at offset zero.
struct Shown(Ref);

impl std::fmt::Display for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.offset {
            0 => write!(f, "{}", self.0.id),
            offset => write!(f, "{}+{}", self.0.id, offset),
        }
    }
}

/// A lattice column and the space its keys are ids of.
struct Lattice {
    column: Column<u64>,
    space: SpaceId,
}

/// What a scope undoes: copied when one opens, put back when it closes.
#[derive(Clone)]
struct State<C, const SPACES: usize> {
    tables: Vec<Table>,
    /// Per table, the space its non-plain cells name.
    table_space: Vec<SpaceId>,
    spaces: [Space; SPACES],
    client: C,
    /// Per lattice column, the keys whose value rose.
    rises: Vec<Rises>,
    /// The stamp a row written now gets. Bumped by [`Database::advance`], so
    /// `stamp == epoch - 1` reads as "written during the epoch that just
    /// ended".
    epoch: u32,
}

#[derive(Clone)]
struct Space {
    uf: UnionFind,
    /// Ids minted and not equated away.
    live: usize,
    /// Whether an equate happened since the last canonicalization.
    dirty: bool,
    /// Whether an equate was a [`Merge::Contradiction`].
    contradicted: bool,
    /// Ids changed since the last [`Database::take_changed`], possibly
    /// non-canonical. An epoch touches the same id many times, so entries are
    /// deduplicated as they are logged.
    changed: Vec<u32>,
    /// Per id, the `changed_epoch` it was last logged in.
    changed_at: Vec<u32>,
    changed_epoch: u32,
    changed_all: bool,
}

/// The keys of one lattice column whose value rose: on an id that already
/// existed, and on an id minted with it. The two are kept apart because an id
/// minted with a value has only new rows around it, which a client searches
/// from anyway.
#[derive(Clone, Default)]
struct Rises {
    rising: [Vec<u32>; 2],
    /// The same for the epoch [`Database::advance`] closed, canonical and
    /// ascending.
    risen: [Vec<u32>; 2],
}

struct Scope<C, const SPACES: usize> {
    saved: State<C, SPACES>,
    /// Per space, the ids minted or equated under the scope.
    touched: Vec<Vec<u32>>,
}

impl<C: Default, const SPACES: usize> Default for Database<C, SPACES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Default, const SPACES: usize> Database<C, SPACES> {
    pub fn new() -> Self {
        Self {
            state: State {
                tables: Vec::new(),
                table_space: Vec::new(),
                spaces: std::array::from_fn(|_| Space {
                    uf: UnionFind::new(),
                    live: 0,
                    dirty: false,
                    contradicted: false,
                    changed: Vec::new(),
                    changed_at: Vec::new(),
                    changed_epoch: 1,
                    // Nothing has been searched yet, so everything is new.
                    changed_all: true,
                }),
                client: C::default(),
                rises: Vec::new(),
                epoch: 1,
            },
            lattices: Vec::new(),
            scopes: Vec::new(),
            version: 1,
            groups: RefCell::default(),
            reverse: RefCell::default(),
            marks: RefCell::default(),
            stats: Stats::default(),
        }
    }
}

impl<C, const SPACES: usize> Database<C, SPACES> {
    // ---- schema -----------------------------------------------------------

    /// Add `table`, whose cells past its plain columns name ids of `space`,
    /// at the next slot. A table added under a scope goes with it.
    pub fn add_table(&mut self, table: Table, space: SpaceId) -> u32 {
        self.state.tables.push(table);
        self.state.table_space.push(space);
        self.state.tables.len() as u32 - 1
    }

    /// A new lattice column keyed on the ids of `space`. Declared with no
    /// scope open: a scope has nothing to put back for it.
    pub fn add_lattice(&mut self, space: SpaceId, join: Join) -> LatticeId {
        assert!(self.scopes.is_empty(), "declared under a scope");
        self.lattices.push(Lattice {
            column: Column::new(join),
            space,
        });
        self.state.rises.push(Rises::default());
        LatticeId(self.lattices.len() as u32 - 1)
    }

    // ---- ids --------------------------------------------------------------

    /// Mint an id of `space` in a set of its own. References to it carry
    /// offsets reduced by `mask`, the [`crate::unionfind::mask`] of its
    /// carrier; a `pinned` id is its carrier's zero, which survives every
    /// equate, so an id equated with it at an offset reads as that constant.
    #[inline]
    pub fn mint(&mut self, space: SpaceId, mask: u64, pinned: bool) -> u32 {
        let state = &mut self.state.spaces[space.0 as usize];
        let id = state.uf.push(mask, pinned);
        state.live += 1;
        self.stats.adds += 1;
        if let Some(scope) = self.scopes.last_mut() {
            scope.touched[space.0 as usize].push(id);
        }
        self.log_change(space, id);
        id
    }

    /// `reference` as its set's representative plus an offset.
    #[inline]
    pub fn find(&self, space: SpaceId, reference: impl Into<Ref>) -> Ref {
        self.state.spaces[space.0 as usize]
            .uf
            .find(reference.into())
    }

    /// The representative of `id`'s set, whatever offset `id` sits at from
    /// it: the set's pinned id, or the smallest id in it.
    #[inline]
    pub fn root(&self, space: SpaceId, id: u32) -> u32 {
        self.state.spaces[space.0 as usize].uf.root(id)
    }

    /// The union-find of `space`.
    #[inline]
    pub fn partition(&self, space: SpaceId) -> &UnionFind {
        &self.state.spaces[space.0 as usize].uf
    }

    /// The union-find of `space` as the outermost open scope found it.
    pub fn partition_before_scopes(&self, space: SpaceId) -> Option<&UnionFind> {
        let scope = self.scopes.first()?;
        Some(&scope.saved.spaces[space.0 as usize].uf)
    }

    /// Whether an equate of `space` since the outermost open scope (or ever,
    /// with none open) was a [`Merge::Contradiction`].
    #[inline]
    pub fn contradicted(&self, space: SpaceId) -> bool {
        self.state.spaces[space.0 as usize].contradicted
    }

    /// One past the highest id `space` ever minted, live or not: the size an
    /// array indexed by its ids needs.
    #[inline]
    pub fn ids(&self, space: SpaceId) -> usize {
        self.state.spaces[space.0 as usize].uf.len()
    }

    /// Ids minted and not equated away. An id minted under a popped scope stays
    /// allocated, its own representative, but no longer counts.
    #[inline]
    pub fn live(&self, space: SpaceId) -> usize {
        self.state.spaces[space.0 as usize].live
    }

    /// Record `a == b`. Unless they were equal already, the lattice columns
    /// over the space join the absorbed id's value onto the survivor's; the
    /// tables keep naming the absorbed id until [`Self::canonicalize`].
    ///
    /// Two references naming one id at different offsets, or two pinned ids,
    /// are a [`Merge::Contradiction`]: nothing merges, the sets are marked
    /// conflicted, and [`Self::contradicted`] reports it.
    #[inline]
    pub fn equate(&mut self, space: SpaceId, a: Ref, b: Ref) -> Merge {
        let state = &mut self.state.spaces[space.0 as usize];
        let (a, b) = (state.uf.find(a), state.uf.find(b));
        if a == b {
            return Merge::Same;
        }
        let merge = state.uf.union(a, b);
        let Merge::Merged { survivor, absorbed } = merge else {
            if crate::trace_enabled() {
                eprintln!("X {} {}", Shown(a), Shown(b));
            }
            self.version += 1;
            state.contradicted = true;
            self.log_change(space, a.id);
            self.log_change(space, b.id);
            return merge;
        };
        self.version += 1;
        if crate::trace_enabled() {
            eprintln!("U {} {} -> {survivor}", Shown(a), Shown(b));
        }
        state.live -= 1;
        state.dirty = true;
        if let Some(scope) = self.scopes.last_mut() {
            scope.touched[space.0 as usize].push(survivor);
        }
        self.log_change(space, survivor);
        self.stats.merges += 1;
        for at in 0..self.lattices.len() {
            let lattice = &self.lattices[at];
            if lattice.space == space && !lattice.column.is_empty() {
                self.merge(LatticeId(at as u32), absorbed, survivor);
            }
        }
        merge
    }

    /// Whether every table of `space` names every id by its representative.
    #[inline]
    pub fn is_canonical(&self, space: SpaceId) -> bool {
        !self.state.spaces[space.0 as usize].dirty
    }

    /// Rewrite every table of `space` through its union-find, carrying the
    /// offsets the equates put ids at, if anything was equated since the last
    /// call; reports whether it was.
    ///
    /// `report` is cleared, then receives the value of every re-keyed row,
    /// already logged as changed, and every pair of rows that came to share a
    /// key, the later one removed. Equating those pairs' values is how a client
    /// restores a functional dependency, and makes the space need this again.
    /// A re-keyed row `dissolve` answers true for is removed instead.
    pub fn canonicalize(
        &mut self,
        space: SpaceId,
        mut dissolve: Option<Dissolve<'_, C>>,
        report: &mut Repair,
    ) -> bool {
        let State {
            tables,
            table_space,
            spaces,
            client,
            epoch,
            ..
        } = &mut self.state;
        let state = &mut spaces[space.0 as usize];
        if !std::mem::take(&mut state.dirty) {
            // Only an equate un-flattens the union-find, and it leaves the
            // space dirty, so a clean one is already flat.
            return false;
        }
        state.uf.flatten();
        self.version += 1;
        report.rekeyed.clear();
        report.collisions.clear();
        let uf = &state.uf;
        let client = &*client;
        let shift = uf.deltas().map(|delta| Shift {
            delta,
            mask: uf.masks(),
        });
        let mut offer = dissolve
            .as_mut()
            .map(|dissolve| |row: RowView<'_>| dissolve(uf, client, row));
        for (table, &over) in tables.iter_mut().zip(table_space.iter()) {
            if over == space {
                let offer = offer
                    .as_mut()
                    .map(|offer| offer as &mut dyn FnMut(RowView<'_>) -> bool);
                table.repair(uf.parents(), shift, offer, *epoch, report);
            }
        }
        self.stats.repairs += report.rekeyed.len();
        // A row with a re-keyed cell reads differently: a semi-naive round
        // has to see its value as changed, and so does a scope.
        if let Some(scope) = self.scopes.last_mut() {
            scope.touched[space.0 as usize].extend_from_slice(&report.rekeyed);
        }
        for &id in &report.rekeyed {
            self.log_change(space, id);
        }
        true
    }

    // ---- tables -----------------------------------------------------------

    #[inline]
    pub fn tables(&self) -> &[Table] {
        &self.state.tables
    }

    #[inline]
    pub fn table(&self, slot: u32) -> Option<&Table> {
        self.state.tables.get(slot as usize)
    }

    /// Insert a row into the table at `slot`, stamped with the current epoch;
    /// see [`Table::insert`].
    #[inline]
    pub fn insert(
        &mut self,
        slot: u32,
        key: &[u32],
        weights: &[u64],
        value: (u32, u64),
        tag: u32,
    ) -> u32 {
        let epoch = self.state.epoch;
        let row = self.state.tables[slot as usize].insert(key, weights, value, tag, epoch);
        self.version += 1;
        row
    }

    /// Group the rows of the table at `slot` by `column`, or bring the
    /// grouping up to date. Read through [`Self::groups`].
    pub fn prepare_group(&self, slot: u32, column: usize) {
        let Some(table) = self.table(slot) else {
            return;
        };
        let mut groups = self.groups.borrow_mut();
        if groups.built.len() <= slot as usize {
            groups.built.resize_with(slot as usize + 1, Vec::new);
        }
        let columns = &mut groups.built[slot as usize];
        if columns.len() <= column {
            columns.resize_with(column + 1, Default::default);
        }
        let ids = self.ids(self.state.table_space[slot as usize]);
        columns[column].refresh(table, column, ids);
    }

    /// List the rows of the table at `slot` the epoch before wrote, by their
    /// cell in `column`, unless that is already done for the table as it is.
    /// Read through [`Self::groups`].
    pub fn prepare_new_rows(&self, slot: u32, column: usize) {
        let Some(table) = self.table(slot) else {
            return;
        };
        let mut groups = self.groups.borrow_mut();
        let groups = &mut *groups;
        if groups.new_rows.len() <= slot as usize {
            groups.new_rows.resize_with(slot as usize + 1, Vec::new);
        }
        let columns = &mut groups.new_rows[slot as usize];
        if columns.len() <= column {
            columns.resize_with(column + 1, Default::default);
        }
        let NewRows { made, rows } = &mut columns[column];
        let now = (table.version() + 1, self.state.epoch);
        if *made != now {
            *made = now;
            groups.scratch.clear();
            tir_adt::simd::select_eq(table.stamps(), self.state.epoch - 1, &mut groups.scratch);
            rows.clear();
            let cells = table.column(column);
            rows.extend(groups.scratch.iter().map(|&row| (cells[row as usize], row)));
            rows.sort_unstable();
        }
    }

    /// Index the rows of the table at `slot` as a trie over `columns`, or
    /// bring the trie up to date. Read through [`Self::groups`].
    pub fn prepare_trie(&self, slot: u32, columns: &[u8]) {
        let Some(table) = self.table(slot) else {
            return;
        };
        let mut groups = self.groups.borrow_mut();
        if groups.tries.len() <= slot as usize {
            groups.tries.resize_with(slot as usize + 1, Vec::new);
        }
        let tries = &mut groups.tries[slot as usize];
        let at = match tries.iter().position(|trie| trie.columns() == columns) {
            Some(at) => at,
            None => {
                tries.push(Trie::new(columns));
                tries.len() - 1
            }
        };
        tries[at].refresh(table);
    }

    #[inline]
    pub fn groups(&self) -> cell::Ref<'_, Groups> {
        self.groups.borrow()
    }

    // ---- client state -----------------------------------------------------

    /// The client's own state, which scopes undo.
    #[inline]
    pub fn client(&self) -> &C {
        &self.state.client
    }

    #[inline]
    pub fn client_mut(&mut self) -> &mut C {
        &mut self.state.client
    }

    // ---- lattice columns --------------------------------------------------

    #[inline]
    pub fn lattice(&self, lattice: LatticeId) -> &Column<u64> {
        &self.lattices[lattice.0 as usize].column
    }

    /// Join `value` onto what canonical `key` is known to be; see
    /// [`Column::raise`]. A rise is logged against the key.
    #[inline]
    pub fn raise(&mut self, lattice: LatticeId, key: u32, value: u64) -> bool {
        self.write(lattice, key, Fact::Known(value), false, true)
    }

    /// [`Self::raise`] for the value `key` is minted with, which
    /// [`Self::risen`] keeps apart.
    #[inline]
    pub fn seed(&mut self, lattice: LatticeId, key: u32, value: u64) -> bool {
        self.write(lattice, key, Fact::Known(value), true, true)
    }

    /// Join `fact` onto `key`'s entry.
    fn write(
        &mut self,
        lattice: LatticeId,
        key: u32,
        fact: Fact<u64>,
        minted: bool,
        counted: bool,
    ) -> bool {
        let epoch = self.state.epoch;
        let moved = self.lattices[lattice.0 as usize]
            .column
            .write(key, fact, epoch);
        if moved {
            self.log_rise(lattice, key, minted);
            self.stats.raises += usize::from(counted);
        }
        moved
    }

    /// Join `absorbed`'s entry onto `survivor`'s, for ids just equated.
    fn merge(&mut self, lattice: LatticeId, absorbed: u32, survivor: u32) {
        let entry = &mut self.lattices[lattice.0 as usize];
        if let Some(fact) = entry.column.detach(absorbed) {
            self.write(lattice, survivor, fact, false, false);
        }
    }

    /// What `key` is known to be in `lattice`. `None` for unknown and for a
    /// conflict.
    pub fn read(&self, lattice: LatticeId, key: u32) -> Option<u64> {
        let entry = &self.lattices[lattice.0 as usize];
        entry.column.get(self.root(entry.space, key))
    }

    /// Whether `key`'s entry in `lattice` rose during the epoch the last
    /// [`Self::advance`] closed.
    pub fn is_new(&self, lattice: LatticeId, key: u32) -> bool {
        let entry = &self.lattices[lattice.0 as usize];
        entry
            .column
            .is_new(self.root(entry.space, key), self.state.epoch)
    }

    /// The keys whose value rose during the epoch the last [`Self::advance`]
    /// closed, canonical and ascending: on an id that already existed, or with
    /// `minted` on one minted with it.
    #[inline]
    pub fn risen(&self, lattice: LatticeId, minted: bool) -> &[u32] {
        &self.state.rises[lattice.0 as usize].risen[usize::from(minted)]
    }

    /// The lattice columns [`Self::risen`] is not empty for, as a mask over
    /// their ids.
    pub fn rose(&self, minted: bool) -> u64 {
        let rises = self.state.rises.iter().enumerate();
        rises
            .filter(|(_, rises)| !rises.risen[usize::from(minted)].is_empty())
            .fold(0, |mask, (at, _)| mask | 1 << at)
    }

    /// Log that `key`'s value in `lattice` moved.
    fn log_rise(&mut self, lattice: LatticeId, key: u32, minted: bool) {
        self.state.rises[lattice.0 as usize].rising[usize::from(minted)].push(key);
        self.log_change(self.lattices[lattice.0 as usize].space, key);
    }

    // ---- epochs and the change log ----------------------------------------

    /// The stamp of the rows written now; a row stamped one below it was
    /// written in the epoch before.
    #[inline]
    pub fn epoch(&self) -> u32 {
        self.state.epoch
    }

    /// Close the epoch: what it wrote becomes what [`Self::prepare_new_rows`]
    /// and [`Self::risen`] read.
    pub fn advance(&mut self) {
        let State {
            spaces,
            rises,
            epoch,
            ..
        } = &mut self.state;
        *epoch += 1;
        for (rises, Lattice { space, .. }) in rises.iter_mut().zip(&self.lattices) {
            let uf = &spaces[space.0 as usize].uf;
            for list in 0..2 {
                let mut risen = std::mem::take(&mut rises.rising[list]);
                for key in &mut risen {
                    *key = uf.root(*key);
                }
                risen.sort_unstable();
                risen.dedup();
                // The drained list's allocation becomes the next epoch's.
                std::mem::swap(&mut rises.risen[list], &mut risen);
                risen.clear();
                rises.rising[list] = risen;
            }
        }
    }

    /// Drain `space`'s change log: the canonical ids changed since the previous
    /// call, ascending; `None` means "every id".
    pub fn take_changed(&mut self, space: SpaceId) -> Option<Vec<u32>> {
        let state = &mut self.state.spaces[space.0 as usize];
        let all = std::mem::replace(&mut state.changed_all, false);
        let mut changed = std::mem::take(&mut state.changed);
        // Every mark of the drained log goes stale at once.
        state.changed_epoch += 1;
        if all {
            return None;
        }
        for id in &mut changed {
            *id = state.uf.root(*id);
        }
        changed.sort_unstable();
        changed.dedup();
        Some(changed)
    }

    /// Report "everything" from the next [`Self::take_changed`] of `space`.
    pub fn mark_all_changed(&mut self, space: SpaceId) {
        self.state.spaces[space.0 as usize].changed_all = true;
    }

    #[inline]
    fn log_change(&mut self, space: SpaceId, id: u32) {
        let state = &mut self.state.spaces[space.0 as usize];
        let at = id as usize;
        if state.changed_at.len() <= at {
            // Ids are minted one at a time; growing by one each time would
            // make every mint a resize.
            let len = (at + 1).max(state.changed_at.len() * 2);
            state.changed_at.resize(len, 0);
        }
        if state.changed_at[at] != state.changed_epoch {
            state.changed_at[at] = state.changed_epoch;
            state.changed.push(id);
        }
    }

    // ---- reading upward ---------------------------------------------------

    /// Fresh marks over the ids of `space`.
    #[inline]
    pub fn marks(&self, space: SpaceId) -> RefMut<'_, Marks> {
        let mut marks = self.marks.borrow_mut();
        marks.begin(self.ids(space));
        marks
    }

    /// `seeds`, canonicalized, and every id reachable from them in at most
    /// `levels` steps (unbounded when `None`), ascending. A step goes from an
    /// id to the value of every row of the space's tables holding it in a key
    /// column, and never from a pinned id.
    pub fn close_upward(
        &self,
        space: SpaceId,
        seeds: impl IntoIterator<Item = u32>,
        levels: Option<usize>,
    ) -> Vec<u32> {
        let reverse = self.reverse(space);
        let uf = self.partition(space);
        let mut seen = self.marks(space);
        let mut frontier: Vec<u32> = seeds
            .into_iter()
            .map(|id| self.root(space, id))
            .filter(|&id| seen.insert(id as usize))
            .collect();
        let mut closure = frontier.clone();
        let mut level = 0;
        while !frontier.is_empty() && levels.is_none_or(|max| level < max) {
            let mut next = Vec::new();
            for id in frontier.drain(..) {
                // A zero's users are every row with that carrier's constant
                // in it. An id equated with it reaches its own users through
                // the rows the equate re-keyed, which are logged themselves.
                if uf.is_zero(id) {
                    continue;
                }
                for &parent in reverse.get(id) {
                    if seen.insert(parent as usize) {
                        closure.push(parent);
                        next.push(parent);
                    }
                }
            }
            frontier = next;
            level += 1;
        }
        closure.sort_unstable();
        closure
    }

    /// Id -> the values of the rows holding it in a key column, canonical,
    /// built once per version. The tables are keyed the other way, so an
    /// upward walk reads this instead.
    fn reverse(&self, space: SpaceId) -> cell::Ref<'_, Csr> {
        let at = space.0 as usize;
        let stale = self
            .reverse
            .borrow()
            .get(at)
            .is_none_or(|(built, _)| *built != self.version);
        if stale {
            let uf = &self.state.spaces[at].uf;
            // A canonical table already names representatives.
            let canonical = self.is_canonical(space);
            let cell = |cell: u32| match canonical {
                true => cell,
                false => uf.root(cell),
            };
            let csr = Csr::build_with(uf.len(), |edge| {
                let tables = self.state.tables.iter().zip(&self.state.table_space);
                for (table, _) in tables.filter(|(_, over)| **over == space) {
                    for column in table.plain_columns()..table.arity() {
                        for (&child, &parent) in table.column(column).iter().zip(table.values()) {
                            edge(cell(child), cell(parent));
                        }
                    }
                }
            });
            let mut reverse = self.reverse.borrow_mut();
            if reverse.len() <= at {
                reverse.resize_with(at + 1, Default::default);
            }
            reverse[at] = (self.version, csr);
        }
        cell::Ref::map(self.reverse.borrow(), |reverse| &reverse[at].1)
    }

    /// How many scopes are open.
    #[inline]
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    /// The ids of `space` minted, equated or re-keyed under the open scopes
    /// from the
    /// `depth`th inward, in no particular order and possibly repeated.
    pub fn scope_touched(&self, space: SpaceId, depth: usize) -> impl Iterator<Item = u32> + '_ {
        self.scopes[depth..]
            .iter()
            .flat_map(move |scope| &scope.touched[space.0 as usize])
            .copied()
    }

    /// Work done since the database was made.
    #[inline]
    pub fn stats(&self) -> Stats {
        self.stats
    }
}

impl<C: Clone, const SPACES: usize> Database<C, SPACES> {
    // ---- scopes -----------------------------------------------------------

    /// Open a scope: everything until the matching [`Self::pop_scope`] is
    /// undone by it.
    pub fn push_scope(&mut self) {
        self.scopes.push(Scope {
            saved: self.state.clone(),
            touched: vec![Vec::new(); SPACES],
        });
        for Lattice { column, .. } in &mut self.lattices {
            column.push_scope();
        }
    }

    /// Close the scope, putting back the tables, partitions, client state,
    /// lattice columns and change log it found. Ids minted under it stay minted, each
    /// in a set of its own, so a caller still holding one can ask about it.
    pub fn pop_scope(&mut self) {
        let scope = self.scopes.pop().expect("open scope");
        for Lattice { column, .. } in &mut self.lattices {
            column.pop_scope();
        }
        let mut saved = scope.saved;
        for (space, now) in saved.spaces.iter_mut().zip(&self.state.spaces) {
            space.uf.extend_to(now.uf.len());
        }
        self.state = saved;
        self.version += 1;
        // Table versions restart from the scope's copy, so a grouping built
        // inside the scope could pass for one of a later table.
        let groups = self.groups.get_mut();
        groups.built.clear();
        groups.new_rows.clear();
        groups.tries.clear();
    }
}

/// Per table and column, the rows grouped by that column; per table, the rows
/// the epoch before wrote; and per table and column order, the trie a join
/// reads.
#[derive(Default)]
pub struct Groups {
    built: Vec<Vec<Group>>,
    new_rows: Vec<Vec<NewRows>>,
    tries: Vec<Vec<Trie>>,
    scratch: Vec<u32>,
}

/// The rows of one table the epoch before wrote, as `(cell, row)` for the
/// cell of one column, ascending.
#[derive(Default)]
struct NewRows {
    /// The table version and epoch the list was made at.
    made: (u64, u32),
    rows: Vec<(u32, u32)>,
}

impl Groups {
    /// The grouping [`Database::prepare_group`] built.
    pub fn get(&self, slot: u32, column: usize) -> &Group {
        &self.built[slot as usize][column]
    }

    /// The rows of the table at `slot` that the epoch before wrote, as
    /// `(cell in column, row)`, ascending; prepared by
    /// [`Database::prepare_new_rows`].
    pub fn new_rows(&self, slot: u32, column: usize) -> &[(u32, u32)] {
        self.new_rows
            .get(slot as usize)
            .and_then(|columns| columns.get(column))
            .map_or(&[], |new| new.rows.as_slice())
    }

    /// The trie [`Database::prepare_trie`] built over `columns`.
    pub(crate) fn trie(&self, slot: u32, columns: &[u8]) -> &Trie {
        let tries = &self.tries[slot as usize];
        let found = tries.iter().find(|trie| trie.columns() == columns);
        found.expect("a prepared trie")
    }

    /// The part of [`Self::new_rows`] whose cell is `cell`.
    pub fn new_rows_with(&self, slot: u32, column: usize, cell: u32) -> &[(u32, u32)] {
        let rows = self.new_rows(slot, column);
        let from = rows.partition_point(|&(other, _)| other < cell);
        let to = rows.partition_point(|&(other, _)| other <= cell);
        &rows[from..to]
    }
}

/// Epoch-stamped membership marks over the ids of a space: `begin` costs
/// nothing per id, so a sweep pays only for what it visits.
#[derive(Default)]
pub struct Marks {
    stamp: Vec<u32>,
    epoch: u32,
}

impl Marks {
    fn begin(&mut self, ids: usize) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.fill(0);
            self.epoch = 1;
        }
        if self.stamp.len() < ids {
            self.stamp.resize(ids, 0);
        }
    }

    /// Mark `id`, reporting whether this sweep had not seen it.
    pub fn insert(&mut self, id: usize) -> bool {
        let slot = &mut self.stamp[id];
        std::mem::replace(slot, self.epoch) != self.epoch
    }
}
