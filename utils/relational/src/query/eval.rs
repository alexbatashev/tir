//! The evaluator: a plan's steps run as a loop nest over a database, with the
//! host consulted for what its parameters resolve to and for its functions.

use smallvec::SmallVec;

use super::ir::{Atom, Cmp, Guard, Host, Lattice, Out, Place, Read, Resolution, Scalar, Var};
use super::join::{JOIN_MIN_ROWS, JoinScratch};
use super::plan::{Arg, FactAnchor, JoinPlan, Plan, RowShape, Shape, Step};
use crate::Ref;
use crate::db::{Database, Groups};
use crate::store::Table;

/// What a client keeps between searches for one plan: what its parameters
/// resolved to, which only changes when the host's version moves.
pub struct PlanCache {
    version: Option<u64>,
    resolutions: Vec<Resolution>,
    /// Whether some row atom of the plan reads no table, so the plan has no
    /// match until a resolution grows.
    dead: bool,
    /// [`JOIN_MIN_ROWS`] unless a test forces a join or the loop nest.
    pub(crate) join_min_rows: usize,
    /// The evaluator's working arrays, kept so a search allocates nothing.
    scratch: Scratch,
    /// A join's working arrays, made by the first join a search runs.
    join: Option<Box<JoinScratch>>,
}

impl PlanCache {
    /// Hand back the matches of a search once they are spent, so the next
    /// search fills their arrays rather than growing new ones.
    pub fn recycle(&mut self, matches: Matches) {
        self.scratch.out = matches;
    }
}

impl Default for PlanCache {
    fn default() -> Self {
        Self {
            version: None,
            resolutions: Vec::new(),
            dead: false,
            join_min_rows: JOIN_MIN_ROWS,
            scratch: Scratch::default(),
            join: None,
        }
    }
}

#[derive(Default)]
struct Scratch {
    bound: Vec<Ref>,
    scalars: Vec<u64>,
    trail: Vec<Var>,
    pool: Vec<Vec<u32>>,
    out: Matches,
}

/// What a variable no atom has reached holds.
pub const UNBOUND: Ref = Ref {
    id: u32::MAX,
    offset: 0,
};

/// `binding` unless it is [`UNBOUND`].
#[inline]
pub fn bound(binding: Ref) -> Option<Ref> {
    (binding.id != UNBOUND.id).then_some(binding)
}

/// Marks a trail entry that put the loose variable back.
const LOOSE: Var = 1 << 31;

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
/// no vector per match. A match is the id the root bound, the reference every
/// variable bound ([`UNBOUND`] for one no atom reached) and the scalars.
#[derive(Default)]
pub struct Matches {
    roots: Vec<u32>,
    /// `roots.len()` rows of bindings back to back, and the same of scalars.
    bindings: Vec<Ref>,
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

    fn push(&mut self, root: u32, bindings: &[Ref], scalars: &[u64]) {
        self.widths = (bindings.len(), scalars.len());
        self.roots.push(root);
        self.bindings.extend_from_slice(bindings);
        self.scalars.extend_from_slice(scalars);
    }

    /// The root, bindings and scalars of match `index`.
    #[inline]
    pub fn get(&self, index: usize) -> (u32, &[Ref], &[u64]) {
        let (vars, scalars) = self.widths;
        (
            self.roots[index],
            &self.bindings[index * vars..(index + 1) * vars],
            &self.scalars[index * scalars..(index + 1) * scalars],
        )
    }

    fn clear(&mut self) {
        self.roots.clear();
        self.bindings.clear();
        self.scalars.clear();
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
            roots.into_iter().map(|root| db.root(space, root)).collect();
        let mut marks = db.marks(space);
        roots.retain(|root| marks.insert(*root as usize));
        drop(marks);
        let scan = roots.len() <= SCAN_LIMIT;
        let mut ready = 0;
        if !scan {
            ready = self.group(db, cache, &self.run, &self.joins);
            for not in &self.nots {
                not.group(db, cache, &not.run, &[]);
            }
        }
        let groups = db.groups();
        let mut eval = self.eval(db, host, &groups, cache, allowed);
        eval.ready = ready;
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
            eval.loosen(self.query.root, root);
            self.step(&mut eval, root, 0);
            eval.bound[self.query.root as usize] = UNBOUND;
            eval.loose = None;
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
        let ready = self.group(db, cache, &anchor.steps[1..], &anchor.joins);
        let groups = db.groups();
        let mut eval = self.eval::<C, S, Unchecked>(db, host, &groups, cache, None);
        eval.ready = ready;
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
        let ready: SmallVec<[u64; 4]> = live
            .iter()
            .map(|&anchor| {
                let anchor = &self.anchors()[anchor];
                match scan {
                    true => 0,
                    false => self.group(db, cache, &anchor.steps[1..], &anchor.joins),
                }
            })
            .collect();
        let groups = db.groups();
        let mut eval = self.eval::<C, S, Unchecked>(db, host, &groups, cache, None);
        eval.scan = scan;
        for (anchor, ready) in live.into_iter().zip(ready) {
            eval.ready = ready;
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
        let ready: SmallVec<[u64; 4]> = self
            .fact_anchors()
            .iter()
            .map(|anchor| match scan {
                true => 0,
                false => self.group(db, cache, &anchor.steps, &anchor.joins),
            })
            .collect();
        let groups = db.groups();
        let mut eval = self.eval::<C, S, Unchecked>(db, host, &groups, cache, None);
        eval.scan = scan;
        eval.only_new = true;
        eval.old_below = usize::MAX;
        for (index, anchor) in self.fact_anchors().iter().enumerate() {
            eval.fact_anchor = Some(index);
            eval.ready = ready[index];
            eval.facts_old = anchor.earlier;
            for &minted in lists(anchor.first_on_key) {
                for &key in db.risen(anchor.column, minted) {
                    eval.loosen(anchor.key, key);
                    self.step(&mut eval, key, 0);
                    eval.bound[anchor.key as usize] = UNBOUND;
                    eval.loose = None;
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
        cache: &'a mut PlanCache,
        allowed: Option<&'a A>,
    ) -> Eval<'a, C, S, A> {
        let mut scratch = std::mem::take(&mut cache.scratch);
        // A lookup finds a representative in the cells, which name the
        // absorbed id until a canonicalization.
        debug_assert!(
            db.is_canonical(self.query.space),
            "search needs a canonical database"
        );
        scratch.bound.clear();
        scratch.bound.resize(self.query.vars as usize, UNBOUND);
        scratch.scalars.clear();
        scratch.scalars.resize(self.query.scalars as usize, 0);
        scratch.trail.clear();
        let mut out = std::mem::take(&mut scratch.out);
        out.clear();
        Eval {
            db,
            host,
            groups,
            resolutions: &cache.resolutions,
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
            join_min_rows: cache.join_min_rows,
            ready: 0,
            join: &mut cache.join,
            loose: None,
            out,
        }
    }

    /// The row atom at `atom`.
    pub(super) fn rows(&self, atom: usize) -> &RowShape {
        match &self.shapes[atom] {
            Shape::Rows(rows) => rows,
            _ => unreachable!("an anchor is a row atom"),
        }
    }

    /// The cells of `table` whose rows also match swapped.
    pub(super) fn when<'r>(
        rows: &RowShape,
        resolutions: &'r [Resolution],
        table: u32,
    ) -> &'r [u32] {
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
        let bind = (rows.tag, rows.key);
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
    /// atom reached through it, by argument for one reached sideways; then
    /// the tries the joins among them read. Reports the joins whose tries it
    /// built, as a mask over their indices.
    fn group<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        cache: &PlanCache,
        steps: &[Step],
        joins: &[JoinPlan],
    ) -> u64 {
        let resolutions = &cache.resolutions;
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
        let mut ready = 0;
        for step in steps {
            if let Step::Join(join) = *step
                && join < 64
                && self.prepare_join(db, resolutions, &joins[join], cache.join_min_rows)
            {
                ready |= 1 << join;
            }
        }
        ready
    }

    pub(super) fn step<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
    ) {
        let steps = match (eval.anchor, eval.fact_anchor) {
            (Some(anchor), _) => &self.anchors()[anchor].steps,
            (None, Some(anchor)) => &self.fact_anchors()[anchor].steps,
            (None, None) => &self.run,
        };
        let mut index = index;
        let step = loop {
            let Some(&step) = steps.get(index) else {
                // A negated conjunction asks only whether a solution exists.
                if eval.counting {
                    eval.hits += 1;
                    return;
                }
                if !eval.only_new || eval.fresh > 0 {
                    let root =
                        bound(eval.bound[self.query.root as usize]).map_or(root, |root| root.id);
                    eval.out.push(root, &eval.bound, &eval.scalars);
                }
                return;
            };
            let Step::Join(join) = step else {
                break step;
            };
            // A join that would not pay is the run of steps that follows.
            if eval.ready >> join.min(63) & 1 == 1 {
                let joins = match (eval.anchor, eval.fact_anchor) {
                    (Some(anchor), _) => &self.anchors()[anchor].joins,
                    (None, Some(anchor)) => &self.fact_anchors()[anchor].joins,
                    (None, None) => &self.joins,
                };
                if self.pays(eval, &joins[join]) {
                    return self.join(eval, root, index, &joins[join]);
                }
            }
            index += 1;
        };
        if eval.counting && eval.hits > 0 {
            return;
        }
        let index_of_atom = match step {
            Step::Parents { atom, slot } => {
                let arg = self.rows(atom).args[slot as usize];
                let child = eval.bound[arg.var as usize];
                debug_assert_ne!(child, UNBOUND, "bound argument");
                self.parents(eval, root, index, atom, arg, child.id);
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
                self.guard(eval, root, index, &self.query.guards[guard]);
                return;
            }
            Step::Join(_) => unreachable!("a join is run or skipped above"),
            Step::Atom(atom) => atom,
        };
        let reference = eval.bound[self.query.atoms[index_of_atom].key() as usize];
        debug_assert_ne!(reference, UNBOUND, "key bound by an earlier step");
        match &self.shapes[index_of_atom] {
            Shape::Rows(rows) => self.value(eval, root, index, index_of_atom, rows, reference.id),
            Shape::Lattice(lattice) => {
                self.lattice(eval, root, index, index_of_atom, lattice, reference)
            }
            Shape::Place(place) => self.place(eval, root, index, place, reference),
        }
    }

    /// Run `guard`, and the steps after it if it holds.
    ///
    /// A rule may write a guard's result into the slot an earlier scalar lives
    /// in. From the root that is harmless: every match recomputes the earlier
    /// one first. An order that starts elsewhere can put a loop over rows
    /// between the two, so what the guard overwrote is put back for the next
    /// row.
    fn guard<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        guard: &Guard,
    ) {
        let written = guard.writes();
        // Most guards write one scalar or none, which needs no list.
        let before: SmallVec<[u64; 2]> = match written {
            [] | [_] => SmallVec::new(),
            _ => written
                .iter()
                .map(|&slot| eval.scalars[slot as usize])
                .collect(),
        };
        let one = written.first().map(|&slot| eval.scalars[slot as usize]);
        if eval.holds(guard) {
            self.step(eval, root, index + 1);
        }
        match (written, one) {
            ([slot], Some(value)) => eval.scalars[*slot as usize] = value,
            _ => {
                for (&slot, value) in written.iter().zip(before) {
                    eval.scalars[slot as usize] = value;
                }
            }
        }
    }

    /// A place atom reads what the id space knows of the reference.
    fn place<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        place: &Place,
        reference: Ref,
    ) {
        let uf = eval.db.partition(self.query.space);
        match place.read {
            Read::Offset { base, offset } => {
                // The offset is read, so it is fixed from here on.
                let mark = eval.trail.len();
                eval.tighten(place.key);
                eval.scalars[offset as usize] = reference.offset;
                if eval.bind_one(base, reference.id.into()) {
                    self.step(eval, root, index + 1);
                }
                eval.unbind(mark);
            }
            Read::Pinned { offset } => {
                let found = uf.find(reference);
                if uf.is_zero(found.id) {
                    let mark = eval.trail.len();
                    eval.tighten(place.key);
                    eval.scalars[offset as usize] = found.offset;
                    self.step(eval, root, index + 1);
                    eval.unbind(mark);
                }
            }
            // An id's carrier never changes, so there is no freshness to
            // track.
            Read::Width { width } => {
                let mask = uf.mask(reference.id);
                if mask != 0 {
                    eval.scalars[width as usize] = u64::from(mask.count_ones());
                    self.step(eval, root, index + 1);
                }
            }
            // An id stops being placed once and for all, so there is no
            // freshness to track either.
            Read::Conflicted => {
                if uf.is_conflicted(reference.id) {
                    self.step(eval, root, index + 1);
                }
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
                    let bind = (rows.tag, rows.key);
                    self.row(eval, root, index, atom, rows, table, row, bind, None, when);
                }
            });
        }
    }

    /// Try one row of an atom's table. `bind` names the scalar that takes the
    /// row's tag and the variable that takes the row's value, or is checked
    /// against it when bound. `swapped` picks the reading to try: as
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
        bind: (Option<Scalar>, Var),
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
            if eval.bind_value(bind.1, table, row)
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
        let bind = (rows.tag, rows.key);
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

    /// A lattice atom reads the value of `key`'s id: the column's stamp says
    /// which epoch proved it.
    fn lattice<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        atom: usize,
        lattice: &Lattice,
        key: Ref,
    ) {
        let db = eval.db;
        if eval.facts_old >> atom.min(63) & 1 == 1 && db.is_new(lattice.column, key.id) {
            return;
        }
        let mark = eval.trail.len();
        let held = match lattice.out {
            // The host read the offset, so it is fixed from here on.
            Out::Host(call) => {
                let held = eval.host.accepts(call, key);
                if held {
                    eval.tighten(lattice.key);
                }
                held
            }
            Out::Word(slot) => db
                .read(lattice.column, key.id)
                .map(|word| eval.scalars[slot as usize] = word)
                .is_some(),
            Out::In(param) => db.read(lattice.column, key.id).is_some_and(|word| {
                eval.resolutions[param as usize]
                    .cells
                    .contains(&(word as u32))
            }),
        };
        if !held {
            return;
        }
        let fresh = usize::from(db.is_new(lattice.column, key.id));
        eval.fresh += fresh;
        self.step(eval, root, index + 1);
        eval.fresh -= fresh;
        eval.unbind(mark);
    }
}

/// Whether `cell` is one of `cells`, which is ascending.
pub(super) fn reads(cells: &[u32], cell: u32) -> bool {
    match cells {
        [only] => *only == cell,
        _ => cells.binary_search(&cell).is_ok(),
    }
}

/// State one search threads through the loop nest.
pub(super) struct Eval<'a, C, const S: usize, A: ?Sized> {
    pub(super) db: &'a Database<C, S>,
    host: &'a dyn Host,
    pub(super) groups: &'a Groups,
    /// Per parameter, what it resolved to; shared by the plan and its nots.
    pub(super) resolutions: &'a [Resolution],
    /// Whether a negated sub-conjunction is being stepped.
    pub(super) nested: bool,
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
    pub(super) old_below: usize,
    /// `None` allows every binding.
    pub(super) allowed: Option<&'a A>,
    only_new: bool,
    /// Inside a negated conjunction: count solutions and stop at the first,
    /// rather than emit them.
    counting: bool,
    hits: usize,
    /// Per variable, its reference or [`UNBOUND`].
    pub(super) bound: Vec<Ref>,
    pub(super) scalars: Vec<u64>,
    /// Variables this branch bound, to undo on the way out. One with
    /// [`LOOSE`] set was the loose variable and had its offset fixed.
    pub(super) trail: Vec<Var>,
    /// The variable the search started at, bound to its id at offset zero
    /// until an atom fixes its offset: a row atom to the row's value, or one
    /// that reads the offset to the offset it read.
    loose: Option<Var>,
    /// Whether rows are looked up by scanning, with no grouping built.
    pub(super) scan: bool,
    /// Vectors a scan's hits go in, reused.
    pool: Vec<Vec<u32>>,
    /// How many rows and values of the partial match the previous epoch
    /// touched.
    pub(super) fresh: usize,
    pub(super) join_min_rows: usize,
    /// The joins of the order being stepped whose tries are built, as a mask
    /// over their indices: the rest run as the loop nest.
    ready: u64,
    pub(super) join: &'a mut Option<Box<JoinScratch>>,
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
            out: Matches::default(),
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

    /// Start a search at `id`, with `var` bound to it and loose.
    fn loosen(&mut self, var: Var, id: u32) {
        self.bound[var as usize] = id.into();
        self.loose = Some(var);
    }

    /// Fix the offset of `var` where it is, if it is the loose variable.
    fn tighten(&mut self, var: Var) {
        if self.loose == Some(var) {
            self.loose = None;
            self.trail.push(var | LOOSE);
        }
    }

    /// Bind `var` to the value of a table row, or check it against the value
    /// when bound.
    #[inline(always)]
    pub(super) fn bind_value(&mut self, var: Var, table: &Table, row: u32) -> bool {
        let id = table.values()[row as usize];
        if !table.weighted() {
            // A row looked up by its value, in a table without weights, has
            // the value the variable already holds at offset zero.
            let prior = self.bound[var as usize];
            let settled = prior.id == id && prior.offset == 0 && self.loose != Some(var);
            return settled || self.bind_one(var, id.into());
        }
        self.bind_one(var, Ref::new(id, table.weight(table.arity(), row)))
    }

    /// Bind a row atom's arguments to the cells of a table row, read swapped
    /// or not, or report the row inconsistent with what is already bound.
    /// Whatever it bound before failing is on the trail.
    #[inline(always)]
    pub(super) fn bind_row(
        &mut self,
        args: &[Arg],
        table: &Table,
        row: u32,
        swapped: bool,
    ) -> bool {
        let weighted = table.weighted();
        for arg in args {
            let column = if swapped { arg.swapped } else { arg.column } as usize;
            // A canonical table names every id by its representative.
            let id = table.column(column)[row as usize];
            let cell = match weighted {
                true => Ref::new(id, table.weight(column, row)),
                false => id.into(),
            };
            if !self.bind_one(arg.var, cell) {
                return false;
            }
        }
        true
    }

    #[inline(always)]
    pub(super) fn bind_one(&mut self, var: Var, reference: Ref) -> bool {
        let prior = self.bound[var as usize];
        // A variable shared by two atoms must bind the same reference.
        if prior == reference {
            return true;
        }
        if prior.id != UNBOUND.id {
            if self.loose != Some(var) || prior.id != reference.id {
                return false;
            }
            self.bound[var as usize] = reference;
            self.loose = None;
            self.trail.push(var | LOOSE);
            return true;
        }
        if self
            .allowed
            .is_some_and(|allowed| !allowed(var, reference.id))
        {
            return false;
        }
        self.bound[var as usize] = reference;
        self.trail.push(var);
        true
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
                let mut words: SmallVec<[u64; 4]> = SmallVec::new();
                for arg in args {
                    let Some(value) = arg.eval(&self.scalars) else {
                        return false;
                    };
                    words.push(value as u64);
                }
                let mut values: SmallVec<[u64; 2]> = SmallVec::from_elem(0, out.len());
                if !self.host.call(*call, &words, &mut values) {
                    return false;
                }
                for (&slot, value) in out.iter().zip(values) {
                    self.scalars[slot as usize] = value;
                }
                true
            }
        }
    }

    pub(super) fn unbind(&mut self, mark: usize) {
        while self.trail.len() > mark {
            let var = self.trail.pop().expect("above the mark");
            if var & LOOSE == 0 {
                self.bound[var as usize] = UNBOUND;
                continue;
            }
            let var = var & !LOOSE;
            self.bound[var as usize] = self.bound[var as usize].id.into();
            self.loose = Some(var);
        }
    }
}
