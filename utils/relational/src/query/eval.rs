//! The evaluator: a plan's steps run as a loop nest over a database, with the
//! host consulted for what its parameters resolve to and for its functions.

use smallvec::SmallVec;

use super::ir::{Atom, Cmp, Guard, Host, Lattice, Out, Resolution, Scalar, Var};
use super::plan::{Arg, FactAnchor, Plan, RowShape, Shape, Step};
use crate::db::{Database, Groups};
use crate::store::Table;

/// What a client keeps between searches for one plan: what its parameters
/// resolved to, which only changes when the host's version moves.
#[derive(Default)]
pub struct PlanCache {
    version: Option<u64>,
    resolutions: Vec<Resolution>,
    /// Whether some row atom of the plan reads no table, so the plan has no
    /// match until a resolution grows.
    dead: bool,
    /// The evaluator's working arrays, kept so a search allocates nothing.
    scratch: Scratch,
}

#[derive(Default)]
struct Scratch {
    bound: Vec<Option<u32>>,
    scalars: Vec<u64>,
    trail: Vec<Var>,
    pool: Vec<Vec<u32>>,
}

/// What prunes a binding: `allowed(var, id)` rejects binding `var` to `id`.
pub trait Allow: Fn(Var, u32) -> bool {}

impl<F: Fn(Var, u32) -> bool + ?Sized> Allow for F {}

/// The searches no caller prunes.
pub(crate) type Unchecked = dyn Fn(Var, u32) -> bool;

/// A search expecting at most this many starting points looks rows up by
/// scanning the column instead of through a grouping: the scans cost less than
/// building one.
const SCAN_LIMIT: usize = 16;

/// Run `$body` for each row of `$table` holding `$cell` in `$column`: read off
/// the grouping, or found by a scan when the search is too small to pay for
/// one.
macro_rules! lookup {
    ($eval:ident, $groups:ident, $slot:expr, $column:expr, $table:ident, $cell:expr, |$row:ident| $body:block) => {
        if $eval.scan {
            let rows = $eval.scan_rows($table.column($column), $cell);
            for &$row in &rows $body
            $eval.pool.push(rows);
        } else {
            for &$row in $groups.get($slot, $column).rows($cell) $body
        }
    };
}

/// The matches of one search, stored flat: a search that finds thousands builds
/// no vector per match. A match is the id the root bound, the id every
/// variable bound (`None` for one no atom reached) and the scalars.
#[derive(Default)]
pub struct Matches {
    roots: Vec<u32>,
    /// `roots.len()` rows of bindings back to back, and the same of scalars.
    bindings: Vec<Option<u32>>,
    scalars: Vec<u64>,
    /// The length of a row of each.
    widths: (usize, usize),
}

impl Matches {
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    fn push(&mut self, root: u32, bindings: &[Option<u32>], scalars: &[u64]) {
        self.widths = (bindings.len(), scalars.len());
        self.roots.push(root);
        self.bindings.extend_from_slice(bindings);
        self.scalars.extend_from_slice(scalars);
    }

    /// The root, bindings and scalars of match `index`.
    pub fn get(&self, index: usize) -> (u32, &[Option<u32>], &[u64]) {
        let (vars, scalars) = self.widths;
        (
            self.roots[index],
            &self.bindings[index * vars..(index + 1) * vars],
            &self.scalars[index * scalars..(index + 1) * scalars],
        )
    }

    /// Every match's bindings, back to back in match order.
    pub fn bindings(&self) -> &[Option<u32>] {
        &self.bindings
    }

    /// Append the matches of another search of the same plan.
    pub fn extend(&mut self, other: Matches) {
        if !other.is_empty() {
            self.widths = other.widths;
        }
        self.roots.extend(other.roots);
        self.bindings.extend(other.bindings);
        self.scalars.extend(other.scalars);
    }
}

impl PlanCache {
    /// What parameter `param` resolved to at the last refresh.
    pub(crate) fn resolution(&self, param: u32) -> &Resolution {
        &self.resolutions[param as usize]
    }
}

impl Plan {
    /// Bring `cache` up to the host's version: what each parameter resolves
    /// to, from scratch the first time and from what appeared since after
    /// that. Reports whether the plan can match at all.
    pub fn refresh<H: Host + ?Sized>(&self, host: &H, cache: &mut PlanCache) -> bool {
        let version = host.version();
        if cache.version == Some(version) {
            return !cache.dead;
        }
        let since = cache.version;
        let params = self.query.params as usize;
        cache.resolutions.resize_with(params, Resolution::default);
        for (param, out) in cache.resolutions.iter_mut().enumerate() {
            host.resolve(param as u32, since, out);
        }
        cache.version = Some(version);
        let resolutions = &cache.resolutions;
        cache.dead = self.query.atoms.iter().any(|atom| {
            matches!(atom, Atom::Rows(rows)
                if resolutions[rows.select.param as usize].tables.is_empty())
        });
        !cache.dead
    }

    /// Every match at `roots`, each root canonicalized and visited once in the
    /// order first given, with every row atom below `fresh.1` held to rows the
    /// previous epoch did not write.
    ///
    /// `allowed(var, id)` prunes a binding the caller rejects. When `fresh.0`
    /// is set, a match nothing of whose rows or values the previous epoch
    /// touched is dropped: it existed an epoch earlier at a root that was
    /// searched then.
    pub fn search_roots<C, const S: usize, A: Allow + ?Sized>(
        &self,
        db: &Database<C, S>,
        host: &dyn Host,
        roots: impl IntoIterator<Item = u32>,
        allowed: Option<&A>,
        fresh: (bool, usize),
        cache: &mut PlanCache,
    ) -> Matches {
        if !self.refresh(host, cache) {
            return Matches::default();
        }
        let space = self.query.space;
        let mut roots: SmallVec<[u32; 16]> =
            roots.into_iter().map(|root| db.find(space, root)).collect();
        let mut marks = db.marks(space);
        roots.retain(|root| marks.insert(*root as usize));
        drop(marks);
        let scan = roots.len() <= SCAN_LIMIT;
        if !scan {
            self.group(db, &cache.resolutions, &self.steps);
            for not in &self.nots {
                not.group(db, &cache.resolutions, &not.steps);
            }
        }
        let groups = db.groups();
        let scratch = std::mem::take(&mut cache.scratch);
        let mut eval = self.eval(db, host, &groups, &cache.resolutions, allowed, scratch);
        eval.scan = scan;
        eval.old_below = fresh.1;
        eval.only_new = fresh.0
            && self
                .query
                .atoms
                .iter()
                .any(|atom| matches!(atom, Atom::Rows(_)));
        for root in roots {
            if allowed.is_some_and(|allowed| !allowed(self.query.root, root)) {
                continue;
            }
            eval.bound[self.query.root as usize] = Some(root);
            self.step(&mut eval, root, 0);
            eval.bound[self.query.root as usize] = None;
        }
        let (out, scratch) = eval.finish();
        cache.scratch = scratch;
        out
    }

    /// Every match, found by scanning the root atom's tables rather than by
    /// visiting ids. For an [`Self::anchorable`] plan.
    pub fn search_all<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        host: &dyn Host,
        cache: &mut PlanCache,
    ) -> Matches {
        if !self.refresh(host, cache) {
            return Matches::default();
        }
        let root = self
            .anchors()
            .iter()
            .position(|anchor| self.query.atoms[anchor.atom].key() == self.query.root)
            .expect("an anchorable plan has a row at its root");
        let anchor = &self.anchors()[root];
        let rows = self.rows(anchor.atom);
        // The scan reads the root's rows by selected cell; the rest are
        // looked up.
        for (table, _) in &cache.resolutions[rows.select.param as usize].tables {
            db.prepare_group(*table, rows.select.column as usize);
        }
        self.group(db, &cache.resolutions, &anchor.steps[1..]);
        let groups = db.groups();
        let scratch = std::mem::take(&mut cache.scratch);
        let mut eval =
            self.eval::<C, S, Unchecked>(db, host, &groups, &cache.resolutions, None, scratch);
        self.scan(&mut eval, root, false);
        let (out, scratch) = eval.finish();
        cache.scratch = scratch;
        out
    }

    /// Every match with a row the previous epoch wrote, each once, found from
    /// those rows. For an [`Self::anchorable`] plan.
    ///
    /// A match with several new rows is found from the first of them in atom
    /// order: the search from a later one holds the earlier atoms to old rows.
    /// A match made new by a lattice value alone is not found here; see
    /// [`Self::search_facts`].
    pub fn search_new<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        host: &dyn Host,
        cache: &mut PlanCache,
    ) -> Matches {
        if !self.refresh(host, cache) {
            return Matches::default();
        }
        // Most epochs give most plans nothing to start from, so that is
        // settled before any grouping is built.
        for anchor in self.anchors() {
            let rows = self.rows(anchor.atom);
            for (table, _) in &cache.resolutions[rows.select.param as usize].tables {
                db.prepare_new_rows(*table, rows.select.column as usize);
            }
        }
        let mut starts = 0;
        let live: SmallVec<[usize; 4]> = {
            let groups = db.groups();
            (0..self.anchors().len())
                .filter(|&anchor| {
                    let rows = self.rows(self.anchors()[anchor].atom);
                    let column = rows.select.column as usize;
                    let found: usize = cache.resolutions[rows.select.param as usize]
                        .tables
                        .iter()
                        .flat_map(|(table, cells)| {
                            cells
                                .iter()
                                .map(|&cell| groups.new_rows_with(*table, column, cell).len())
                        })
                        .sum();
                    starts += found;
                    found != 0
                })
                .collect()
        };
        if live.is_empty() {
            return Matches::default();
        }
        let scan = starts <= SCAN_LIMIT;
        if !scan {
            for &anchor in &live {
                self.group(db, &cache.resolutions, &self.anchors()[anchor].steps[1..]);
            }
        }
        let groups = db.groups();
        let scratch = std::mem::take(&mut cache.scratch);
        let mut eval =
            self.eval::<C, S, Unchecked>(db, host, &groups, &cache.resolutions, None, scratch);
        eval.scan = scan;
        for anchor in live {
            self.scan(&mut eval, anchor, true);
        }
        let (out, scratch) = eval.finish();
        cache.scratch = scratch;
        out
    }

    /// Every match at `roots` whose rows are all older than the previous epoch
    /// and one of whose lattice values is not: what [`Self::search_new`]
    /// leaves out.
    pub fn search_facts<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        host: &dyn Host,
        roots: impl IntoIterator<Item = u32>,
        cache: &mut PlanCache,
    ) -> Matches {
        self.search_roots(
            db,
            host,
            roots,
            None::<&Unchecked>,
            (true, usize::MAX),
            cache,
        )
    }

    /// Every match one of whose lattice values the previous epoch raised and
    /// whose rows are all older than that, each once, found from those values:
    /// what [`Self::search_new`] leaves out. `minted` adds the values ids were
    /// minted with, which only a plan that reads no row needs. For a
    /// [`Self::fact_anchorable`] plan.
    pub fn search_risen<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        host: &dyn Host,
        minted: bool,
        cache: &mut PlanCache,
    ) -> Matches {
        if !self.refresh(host, cache) {
            return Matches::default();
        }
        let lists = |first_on_key: bool| match minted && first_on_key {
            true => &[false, true][..],
            false => &[false][..],
        };
        let starts = |anchor: &FactAnchor| {
            lists(anchor.first_on_key)
                .iter()
                .map(|&minted| db.risen(anchor.column, minted).len())
                .sum::<usize>()
        };
        let total: usize = self.fact_anchors().iter().map(starts).sum();
        if total == 0 {
            return Matches::default();
        }
        let scan = total <= SCAN_LIMIT;
        if !scan {
            for anchor in self.fact_anchors() {
                self.group(db, &cache.resolutions, &anchor.steps);
            }
        }
        let groups = db.groups();
        let scratch = std::mem::take(&mut cache.scratch);
        let mut eval =
            self.eval::<C, S, Unchecked>(db, host, &groups, &cache.resolutions, None, scratch);
        eval.scan = scan;
        eval.only_new = true;
        eval.old_below = usize::MAX;
        for (index, anchor) in self.fact_anchors().iter().enumerate() {
            eval.fact_anchor = Some(index);
            eval.facts_old = anchor.earlier;
            for &minted in lists(anchor.first_on_key) {
                for &key in db.risen(anchor.column, minted) {
                    eval.bound[anchor.key as usize] = Some(key);
                    self.step(&mut eval, key, 0);
                    eval.bound[anchor.key as usize] = None;
                }
            }
        }
        let (out, scratch) = eval.finish();
        cache.scratch = scratch;
        out
    }

    fn eval<'a, C, const S: usize, A: Allow + ?Sized>(
        &self,
        db: &'a Database<C, S>,
        host: &'a dyn Host,
        groups: &'a Groups,
        resolutions: &'a [Resolution],
        allowed: Option<&'a A>,
        mut scratch: Scratch,
    ) -> Eval<'a, C, S, A> {
        // A lookup finds a representative in the cells, which name the
        // absorbed id until a canonicalization.
        debug_assert!(
            db.is_canonical(self.query.space),
            "search needs a canonical database"
        );
        scratch.bound.clear();
        scratch.bound.resize(self.query.vars as usize, None);
        scratch.scalars.clear();
        scratch.scalars.resize(self.query.scalars as usize, 0);
        scratch.trail.clear();
        Eval {
            db,
            host,
            groups,
            resolutions,
            nested: false,
            anchor: None,
            fact_anchor: None,
            facts_old: 0,
            old_below: 0,
            allowed,
            only_new: false,
            counting: false,
            hits: 0,
            bound: scratch.bound,
            scalars: scratch.scalars,
            trail: scratch.trail,
            scan: false,
            pool: scratch.pool,
            fresh: 0,
            out: Matches::default(),
        }
    }

    /// The row atom at `atom`.
    fn rows(&self, atom: usize) -> &RowShape {
        match &self.shapes[atom] {
            Shape::Rows(rows) => rows,
            Shape::Lattice(_) => unreachable!("an anchor is a row atom"),
        }
    }

    /// The cells of `table` whose rows also match swapped.
    fn when<'r>(rows: &RowShape, resolutions: &'r [Resolution], table: u32) -> &'r [u32] {
        match rows.when {
            Some(when) => resolutions[when as usize].cells_of(table),
            None => &[],
        }
    }

    /// Search from the rows of one anchor's atom: the ones the previous epoch
    /// wrote, or all of them.
    fn scan<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        anchor: usize,
        new: bool,
    ) {
        let atom = self.anchors()[anchor].atom;
        let rows = self.rows(atom);
        let (db, groups, resolutions) = (eval.db, eval.groups, eval.resolutions);
        eval.anchor = Some(anchor);
        eval.old_below = if new { atom } else { 0 };
        let column = rows.select.column as usize;
        let bind = (rows.tag, Some(rows.key));
        for (slot, cells) in &resolutions[rows.select.param as usize].tables {
            let Some(table) = db.table(*slot) else {
                continue;
            };
            let when = Self::when(rows, resolutions, *slot);
            for &cell in cells {
                if new {
                    for &(_, found) in groups.new_rows_with(*slot, column, cell) {
                        self.row(eval, 0, 0, atom, rows, table, found, bind, None, when);
                    }
                    continue;
                }
                for &found in groups.get(*slot, column).rows(cell) {
                    self.row(eval, 0, 0, atom, rows, table, found, bind, None, when);
                }
            }
        }
        eval.anchor = None;
    }

    /// Build the grouping each of `steps` looks rows up in: by value for an
    /// atom reached through it, by argument for one reached sideways.
    fn group<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        resolutions: &[Resolution],
        steps: &[Step],
    ) {
        for step in steps {
            let (atom, slot) = match *step {
                Step::Atom(atom) => (atom, None),
                Step::Parents { atom, slot } => (atom, Some(slot)),
                _ => continue,
            };
            let Shape::Rows(rows) = &self.shapes[atom] else {
                continue;
            };
            for (table_slot, _) in &resolutions[rows.select.param as usize].tables {
                let Some(table) = db.table(*table_slot) else {
                    continue;
                };
                match slot {
                    None => db.prepare_group(*table_slot, table.arity()),
                    Some(slot) => {
                        let arg = rows.args[slot as usize];
                        db.prepare_group(*table_slot, arg.column as usize);
                        if rows.when.is_some() {
                            db.prepare_group(*table_slot, arg.swapped as usize);
                        }
                    }
                }
            }
        }
    }

    fn step<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
    ) {
        let steps = match (eval.anchor, eval.fact_anchor) {
            (Some(anchor), _) => &self.anchors()[anchor].steps,
            (None, Some(anchor)) => &self.fact_anchors()[anchor].steps,
            (None, None) => &self.steps,
        };
        let Some(&step) = steps.get(index) else {
            // A negated conjunction asks only whether a solution exists.
            if eval.counting {
                eval.hits += 1;
                return;
            }
            if !eval.only_new || eval.fresh > 0 {
                let root = eval.bound[self.query.root as usize].unwrap_or(root);
                eval.out.push(root, &eval.bound, &eval.scalars);
            }
            return;
        };
        if eval.counting && eval.hits > 0 {
            return;
        }
        let index_of_atom = match step {
            Step::Parents { atom, slot } => {
                let arg = self.rows(atom).args[slot as usize];
                let child = eval.bound[arg.var as usize].expect("bound argument");
                self.parents(eval, root, index, atom, arg, child);
                return;
            }
            Step::Not(not) => {
                let outer = std::mem::replace(&mut eval.counting, true);
                let hits = std::mem::replace(&mut eval.hits, 0);
                let nested = std::mem::replace(&mut eval.nested, true);
                let mark = eval.trail.len();
                self.nots[not].step(eval, root, 0);
                eval.unbind(mark);
                let blocked = eval.hits > 0;
                eval.counting = outer;
                eval.hits = hits;
                eval.nested = nested;
                if !blocked {
                    self.step(eval, root, index + 1);
                }
                return;
            }
            Step::Guard(guard) => {
                // A rule may write a guard's result into the slot an earlier
                // scalar lives in. From the root that is harmless: every match
                // recomputes the earlier one first. An order that starts
                // elsewhere can put a loop over rows between the two, so what
                // the guard overwrote is put back for the next row.
                let guard = &self.query.guards[guard];
                let written = guard.writes();
                let before: SmallVec<[u64; 2]> = written
                    .iter()
                    .map(|&slot| eval.scalars[slot as usize])
                    .collect();
                if eval.holds(guard) {
                    self.step(eval, root, index + 1);
                }
                for (&slot, value) in written.iter().zip(before) {
                    eval.scalars[slot as usize] = value;
                }
                return;
            }
            Step::Atom(atom) => atom,
        };
        match &self.shapes[index_of_atom] {
            Shape::Rows(rows) => {
                let key = eval.bound[rows.key as usize].expect("key bound by an earlier step");
                self.value(eval, root, index, index_of_atom, rows, key)
            }
            Shape::Lattice(lattice) => {
                let key = eval.bound[lattice.key as usize].expect("key bound by an earlier step");
                self.lattice(eval, root, index, index_of_atom, lattice, key)
            }
        }
    }

    /// Every row the atom reads whose value is `value`.
    fn value<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        atom: usize,
        rows: &RowShape,
        value: u32,
    ) {
        let (db, groups, resolutions) = (eval.db, eval.groups, eval.resolutions);
        let select = rows.select.column as usize;
        for (slot, cells) in &resolutions[rows.select.param as usize].tables {
            let Some(table) = db.table(*slot) else {
                continue;
            };
            let when = Self::when(rows, resolutions, *slot);
            lookup!(eval, groups, *slot, table.arity(), table, value, |row| {
                if reads(cells, table.column(select)[row as usize]) {
                    let bind = (rows.tag, None);
                    self.row(eval, root, index, atom, rows, table, row, bind, None, when);
                }
            });
        }
    }

    /// Try one row of an atom's table. `bind` names the scalar that takes the
    /// row's tag and, when the atom was not reached through its value, the
    /// variable that takes the value. `swapped` picks the reading to try: as
    /// written or swapped alone, or with `None` both where `when` holds the
    /// row's selected cell.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn row<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        atom: usize,
        rows: &RowShape,
        table: &Table,
        row: u32,
        bind: (Option<Scalar>, Option<Var>),
        swapped: Option<bool>,
        when: &[u32],
    ) {
        let fresh = usize::from(table.stamps()[row as usize] + 1 == eval.db.epoch());
        if fresh == 1 && !eval.nested && atom < eval.old_below {
            return;
        }
        if let Some(slot) = bind.0 {
            eval.scalars[slot as usize] = table.tags()[row as usize] as u64;
        }
        let readings = match swapped {
            None if !when.is_empty()
                && reads(
                    when,
                    table.column(rows.select.column as usize)[row as usize],
                ) =>
            {
                0..2
            }
            None => 0..1,
            Some(swapped) => usize::from(swapped)..usize::from(swapped) + 1,
        };
        eval.fresh += fresh;
        for reading in readings {
            let mark = eval.trail.len();
            let value = table.values()[row as usize];
            if bind.1.is_none_or(|var| eval.bind_one(var, value))
                && eval.bind_row(&rows.args, table, row, reading == 1)
            {
                self.step(eval, root, index + 1);
            }
            eval.unbind(mark);
        }
        eval.fresh -= fresh;
    }

    /// Reach a row through a bound argument: the rows the atom reads that hold
    /// `child` in `arg`'s column. Everything already bound is checked against
    /// the row; the rest, including the row's value, is bound from it.
    fn parents<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        atom: usize,
        arg: Arg,
        child: u32,
    ) {
        let (db, groups, resolutions) = (eval.db, eval.groups, eval.resolutions);
        let rows = self.rows(atom);
        let select = rows.select.column as usize;
        let bind = (rows.tag, Some(rows.key));
        for (slot, cells) in &resolutions[rows.select.param as usize].tables {
            let Some(table) = db.table(*slot) else {
                continue;
            };
            let when = Self::when(rows, resolutions, *slot);
            // Read swapped, the bound argument sits in the other column. No
            // row of a table with no cell in `when` reads swapped.
            let readings = if when.is_empty() { 1 } else { 2 };
            for reading in 0..readings {
                let at = if reading == 1 {
                    arg.swapped
                } else {
                    arg.column
                } as usize;
                lookup!(eval, groups, *slot, at, table, child, |row| {
                    let cell = table.column(select)[row as usize];
                    if reads(cells, cell) && (reading == 0 || reads(when, cell)) {
                        let swapped = Some(reading == 1);
                        self.row(
                            eval, root, index, atom, rows, table, row, bind, swapped, when,
                        );
                    }
                });
            }
        }
    }

    /// A lattice atom reads `key`'s value: the column's stamp says which epoch
    /// proved it.
    fn lattice<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        atom: usize,
        lattice: &Lattice,
        key: u32,
    ) {
        let db = eval.db;
        if eval.facts_old >> atom.min(63) & 1 == 1 && db.is_new(lattice.column, key) {
            return;
        }
        let Some(words) = db.read(lattice.column, key) else {
            return;
        };
        let mark = eval.trail.len();
        for (out, &word) in lattice.out.iter().zip(&words) {
            let held = match *out {
                Out::Var(var) => eval.bind_one(var, db.find(self.query.space, word as u32)),
                Out::Word(slot) => {
                    eval.scalars[slot as usize] = word;
                    true
                }
                Out::In(param) => eval.resolutions[param as usize]
                    .cells
                    .contains(&(word as u32)),
            };
            if !held {
                eval.unbind(mark);
                return;
            }
        }
        let fresh = usize::from(db.is_new(lattice.column, key));
        eval.fresh += fresh;
        self.step(eval, root, index + 1);
        eval.fresh -= fresh;
        eval.unbind(mark);
    }
}

/// Whether `cell` is one of `cells`, which is ascending.
fn reads(cells: &[u32], cell: u32) -> bool {
    match cells {
        [only] => *only == cell,
        _ => cells.binary_search(&cell).is_ok(),
    }
}

/// State one search threads through the loop nest.
struct Eval<'a, C, const S: usize, A: ?Sized> {
    db: &'a Database<C, S>,
    host: &'a dyn Host,
    groups: &'a Groups,
    /// Per parameter, what it resolved to; shared by the plan and its nots.
    resolutions: &'a [Resolution],
    /// Whether a negated sub-conjunction is being stepped.
    nested: bool,
    /// The anchor whose order is being stepped, when the search started at a
    /// row atom rather than at the root.
    anchor: Option<usize>,
    /// The fact anchor whose order is being stepped, when the search started
    /// at a risen value.
    fact_anchor: Option<usize>,
    /// The lattice atoms, as a mask over atom indices, that match only values
    /// older than the previous epoch.
    facts_old: u64,
    /// Row atoms below this index match only rows older than the previous
    /// epoch.
    old_below: usize,
    /// `None` allows every binding.
    allowed: Option<&'a A>,
    only_new: bool,
    /// Inside a negated conjunction: count solutions and stop at the first,
    /// rather than emit them.
    counting: bool,
    hits: usize,
    bound: Vec<Option<u32>>,
    scalars: Vec<u64>,
    /// Variables this branch bound, to undo on the way out.
    trail: Vec<Var>,
    /// Whether rows are looked up by scanning, with no grouping built.
    scan: bool,
    /// Vectors a scan's hits go in, reused.
    pool: Vec<Vec<u32>>,
    /// How many rows and values of the partial match the previous epoch
    /// touched.
    fresh: usize,
    out: Matches,
}

impl<C, const S: usize, A: Allow + ?Sized> Eval<'_, C, S, A> {
    /// The matches found, and the working arrays for the next search.
    fn finish(self) -> (Matches, Scratch) {
        let scratch = Scratch {
            bound: self.bound,
            scalars: self.scalars,
            trail: self.trail,
            pool: self.pool,
        };
        (self.out, scratch)
    }

    /// The rows of `column` holding `cell`, in a vector from the pool.
    #[inline]
    fn scan_rows(&mut self, column: &[u32], cell: u32) -> Vec<u32> {
        let mut rows = self.pool.pop().unwrap_or_default();
        rows.clear();
        tir_adt::simd::select_eq(column, cell, &mut rows);
        rows
    }

    /// Bind a row atom's arguments to the cells of a table row, read swapped
    /// or not, or report the row inconsistent with what is already bound.
    /// Whatever it bound before failing is on the trail.
    #[inline]
    fn bind_row(&mut self, args: &[Arg], table: &Table, row: u32, swapped: bool) -> bool {
        args.iter().all(|arg| {
            let column = if swapped { arg.swapped } else { arg.column };
            // A canonical table names every id by its representative.
            self.bind_one(arg.var, table.column(column as usize)[row as usize])
        })
    }

    #[inline]
    fn bind_one(&mut self, var: Var, id: u32) -> bool {
        match self.bound[var as usize] {
            // A variable shared by two atoms must bind the same id.
            Some(prior) => prior == id,
            None => {
                if self.allowed.is_some_and(|allowed| !allowed(var, id)) {
                    return false;
                }
                self.bound[var as usize] = Some(id);
                self.trail.push(var);
                true
            }
        }
    }

    /// Whether `guard` holds of the scalars bound so far, binding its own
    /// output if it does.
    fn holds(&mut self, guard: &Guard) -> bool {
        match guard {
            Guard::Cmp(cmp, a, b) => {
                let (Some(a), Some(b)) = (a.eval(&self.scalars), b.eval(&self.scalars)) else {
                    return false;
                };
                match cmp {
                    Cmp::Lt => a < b,
                    Cmp::Le => a <= b,
                    Cmp::Eq => a == b,
                    Cmp::Ne => a != b,
                }
            }
            Guard::Distinct(pairs) => !pairs
                .iter()
                .all(|&(a, b)| self.bound[a as usize] == self.bound[b as usize]),
            Guard::Let { out, value } => {
                let Some(value) = value.eval(&self.scalars) else {
                    return false;
                };
                self.scalars[*out as usize] = value as u64;
                true
            }
            Guard::Extern { call, args, out } => {
                let Some(args): Option<SmallVec<[u64; 4]>> = args
                    .iter()
                    .map(|arg| arg.eval(&self.scalars).map(|value| value as u64))
                    .collect()
                else {
                    return false;
                };
                let mut values: SmallVec<[u64; 2]> = SmallVec::from_elem(0, out.len());
                if !self.host.call(*call, &args, &mut values) {
                    return false;
                }
                for (&slot, value) in out.iter().zip(values) {
                    self.scalars[slot as usize] = value;
                }
                true
            }
        }
    }

    fn unbind(&mut self, mark: usize) {
        for var in self.trail.drain(mark..) {
            self.bound[var as usize] = None;
        }
    }
}
