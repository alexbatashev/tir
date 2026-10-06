//! The executor of a [`Step::Join`]: a worst-case optimal join over tries,
//! which binds the run's variables one at a time by intersecting the cells
//! every atom reading the variable allows, and then hands the run's matches
//! on in the order the loop nest would have found them.

use smallvec::SmallVec;

use super::eval::{Allow, Eval, reads};
use super::ir::Resolution;
use super::plan::{JoinAtom, JoinPlan, Level, Plan, Step};
use super::set::Set;
use super::trie::{Node, Trie};
use crate::db::{Database, Groups};
use crate::store::{Group, Table};

/// A join runs as the loop nest unless its first atom reaches at least this
/// many rows, and its tries are built only where the tables that atom reads
/// hold at least this many rows per cell: below that, probing each row costs
/// less than building and intersecting sets.
pub(crate) const JOIN_MIN_ROWS: usize = 32;

/// A join's working arrays, kept so a search allocates nothing.
#[derive(Default)]
pub(super) struct JoinScratch {
    /// The cursors of every frame, stacked by depth, each frame's atom by
    /// atom.
    cursors: Vec<Cursor>,
    /// Per frame, where each atom's cursors start and where the frame ends.
    spans: Vec<u32>,
    /// Per depth, the cells every atom allows there.
    candidates: Vec<Vec<u32>>,
    /// Lists the cells of an atom's several tries are merged into.
    unions: Vec<Vec<u32>>,
    words: Vec<u64>,
    spare: Vec<u32>,
    /// Per atom, the ranks of the rows its leaves hold.
    leaves: Vec<Vec<u64>>,
    /// The ranks of a run's matches and the order they go in, one pair per
    /// join being bound.
    tuples: Vec<(Vec<u64>, Vec<u32>)>,
}

/// Where an atom's relation is read: under one selected cell of one table,
/// as written or swapped.
#[derive(Clone, Copy)]
struct Cursor {
    /// Index into the join's sources.
    source: u32,
    node: Node,
}

/// One table an atom reads, in one reading.
struct Source<'a> {
    trie: &'a Trie,
    table: &'a Table,
    /// The rank of the table's row zero; a row's rank is this with the row
    /// shifted left by `shift`.
    base: u64,
    shift: u32,
    /// Whether the atom holds to rows the previous epoch did not write.
    old: bool,
}

/// A row's rank in the loop nest's order of an atom's rows: the table's
/// position in the selection first, then the row and the reading, or for a
/// sideways step, which looks each reading up in its own column, the reading
/// and the row.
fn rank(table: usize, reading: usize, sideways: bool) -> (u64, u32) {
    let table = (table as u64) << 33;
    match sideways {
        true => (table | (reading as u64) << 32, 0),
        false => (table | reading as u64, 1),
    }
}

/// The table position, row and reading of a rank.
fn unrank(rank: u64, sideways: bool) -> (usize, u32, bool) {
    let table = (rank >> 33) as usize;
    match sideways {
        true => (table, rank as u32, rank >> 32 & 1 == 1),
        false => (table, (rank >> 1) as u32, rank & 1 == 1),
    }
}

impl Plan {
    /// What `atom`, the first of a join, reaches through its step: `count`
    /// summed over the groupings it looks rows up in, both readings included.
    pub(super) fn reach<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        groups: &Groups,
        resolutions: &[Resolution],
        atom: &JoinAtom,
        count: impl Fn(&Group) -> usize,
    ) -> usize {
        let rows = self.rows(atom.atom());
        let tables = &resolutions[rows.select.param as usize].tables;
        tables
            .iter()
            .map(|(slot, _)| {
                let Some(table) = db.table(*slot) else {
                    return 0;
                };
                let Step::Parents { slot: arg, .. } = atom.step else {
                    return count(groups.get(*slot, table.arity()));
                };
                let arg = rows.args[arg as usize];
                let swapped = match Self::when(rows, resolutions, *slot).is_empty() {
                    true => 0,
                    false => count(groups.get(*slot, arg.swapped as usize)),
                };
                count(groups.get(*slot, arg.column as usize)) + swapped
            })
            .sum()
    }

    /// Build the tries a join reads, unless the tables its first atom looks
    /// rows up in hold too few rows per cell for a join to pay. Reports
    /// whether it built them.
    pub(super) fn prepare_join<C, const S: usize>(
        &self,
        db: &Database<C, S>,
        resolutions: &[Resolution],
        join: &JoinPlan,
        min_rows: usize,
    ) -> bool {
        let first = &join.atoms[0];
        let (rows, cells) = {
            let groups = db.groups();
            let rows = self.reach(db, &groups, resolutions, first, Group::len);
            (
                rows,
                self.reach(db, &groups, resolutions, first, Group::cells),
            )
        };
        if rows < min_rows.saturating_mul(cells.max(1)) {
            return false;
        }
        for atom in &join.atoms {
            let rows = self.rows(atom.atom());
            for (slot, _) in &resolutions[rows.select.param as usize].tables {
                db.prepare_trie(*slot, &atom.columns[0]);
                if !atom.columns[1].is_empty() && !Self::when(rows, resolutions, *slot).is_empty() {
                    db.prepare_trie(*slot, &atom.columns[1]);
                }
            }
        }
        true
    }

    /// Whether the join, its tries built, is worth running rather than its
    /// run's steps: whether its first atom reaches enough rows.
    #[inline(never)]
    pub(super) fn pays<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &Eval<'_, C, S, A>,
        join: &JoinPlan,
    ) -> bool {
        let first = &join.atoms[0];
        let key = match first.step {
            Step::Parents { atom, slot } => self.rows(atom).args[slot as usize].var,
            _ => self.rows(first.atom()).key,
        };
        let key = eval.bound[key as usize].expect("a step's key is bound");
        let count = |group: &Group| group.rows(key).len();
        self.reach(eval.db, eval.groups, eval.resolutions, first, count) >= eval.join_min_rows
    }

    /// Run the join at step `index`, then the steps after its run, once per
    /// match of the run, in the loop nest's order.
    // Out of line, so the loop nest's step stays as small as it was.
    #[inline(never)]
    pub(super) fn join<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        root: u32,
        index: usize,
        join: &JoinPlan,
    ) {
        let (mut tuples, mut order) = eval
            .join
            .get_or_insert_default()
            .tuples
            .pop()
            .unwrap_or_default();
        tuples.clear();
        self.enumerate(eval, join, &mut tuples);
        // The loop nest meets the run's matches ordered by each atom's row
        // rank in step order, which is the order of the rank tuples.
        let width = join.atoms.len();
        order.clear();
        order.extend(0..(tuples.len() / width) as u32);
        let rows = |at: u32| &tuples[at as usize * width..(at as usize + 1) * width];
        order.sort_unstable_by(|&a, &b| rows(a).cmp(rows(b)));
        let (db, resolutions) = (eval.db, eval.resolutions);
        for &at in &order {
            let mark = eval.trail.len();
            let mut fresh = 0;
            let mut held = true;
            for (atom, &rank) in join.atoms.iter().zip(rows(at)) {
                let shape = self.rows(atom.atom());
                let sideways = matches!(atom.step, Step::Parents { .. });
                let (table, row, swapped) = unrank(rank, sideways);
                let slot = resolutions[shape.select.param as usize].tables[table].0;
                let table = db.table(slot).expect("a joined table");
                if let Some(tag) = shape.tag {
                    eval.scalars[tag as usize] = table.tags()[row as usize] as u64;
                }
                fresh += usize::from(table.stamps()[row as usize] + 1 == db.epoch());
                held = held
                    && eval.bind_one(shape.key, table.values()[row as usize])
                    && eval.bind_row(&shape.args, table, row, swapped);
            }
            debug_assert!(held, "a joined row agrees with the rest");
            if held {
                eval.fresh += fresh;
                self.step(eval, root, index + 1 + join.len);
                eval.fresh -= fresh;
            }
            eval.unbind(mark);
        }
        eval.join
            .get_or_insert_default()
            .tuples
            .push((tuples, order));
    }

    /// Append the rank tuple of every match of the join's run to `out`, in no
    /// particular order.
    fn enumerate<C, const S: usize, A: Allow + ?Sized>(
        &self,
        eval: &mut Eval<'_, C, S, A>,
        join: &JoinPlan,
        out: &mut Vec<u64>,
    ) {
        let (db, groups, resolutions) = (eval.db, eval.groups, eval.resolutions);
        let scratch = &mut **eval.join.get_or_insert_default();
        scratch.cursors.clear();
        scratch.spans.clear();
        let mut sources: SmallVec<[Source<'_>; 16]> = SmallVec::new();
        for atom in &join.atoms {
            scratch.spans.push(scratch.cursors.len() as u32);
            let rows = self.rows(atom.atom());
            let sideways = matches!(atom.step, Step::Parents { .. });
            let old = !eval.nested && atom.atom() < eval.old_below;
            let tables = &resolutions[rows.select.param as usize].tables;
            for (position, (slot, cells)) in tables.iter().enumerate() {
                let Some(table) = db.table(*slot) else {
                    continue;
                };
                let when = Self::when(rows, resolutions, *slot);
                let readings = match atom.columns[1].is_empty() || when.is_empty() {
                    true => 1,
                    false => 2,
                };
                for reading in 0..readings {
                    let trie = groups.trie(*slot, &atom.columns[reading]);
                    let (base, shift) = rank(position, reading, sideways);
                    sources.push(Source {
                        trie,
                        table,
                        base,
                        shift,
                        old,
                    });
                    let source = sources.len() as u32 - 1;
                    for &cell in cells {
                        if reading == 1 && !reads(when, cell) {
                            continue;
                        }
                        let found = atom.levels.iter().try_fold(
                            trie.child(trie.root(), cell),
                            |node, level| match level {
                                Level::Known(var) => {
                                    let cell = eval.bound[*var as usize].expect("bound before");
                                    Ok(node.and_then(|node| trie.child(node, cell)))
                                }
                                Level::Attr(_) => Err(node),
                            },
                        );
                        if let Some(node) = found.unwrap_or_else(|node| node) {
                            scratch.cursors.push(Cursor { source, node });
                        }
                    }
                }
            }
            if scratch.cursors.len() as u32 == *scratch.spans.last().unwrap() {
                return;
            }
        }
        scratch.spans.push(scratch.cursors.len() as u32);
        scratch
            .candidates
            .resize_with(join.vars.len().max(scratch.candidates.len()), Vec::new);
        scratch
            .leaves
            .resize_with(join.atoms.len().max(scratch.leaves.len()), Vec::new);
        let run = Run {
            join,
            sources,
            epoch: db.epoch(),
        };
        run.descend(scratch, eval.allowed, 0, out);
    }
}

/// What a join's descent reads and does not change.
struct Run<'r, 'a> {
    join: &'r JoinPlan,
    sources: SmallVec<[Source<'a>; 16]>,
    epoch: u32,
}

impl Run<'_, '_> {
    /// Bind the run's variable at `depth` to each cell every atom reading it
    /// allows, descend the atoms' cursors under it, and go on to the next;
    /// past the last, collect the leaves.
    fn descend<A: Allow + ?Sized>(
        &self,
        scratch: &mut JoinScratch,
        allowed: Option<&A>,
        depth: usize,
        out: &mut Vec<u64>,
    ) {
        let width = self.join.atoms.len();
        let frame = depth * (width + 1);
        let Some(&var) = self.join.vars.get(depth) else {
            return self.leaves(scratch, frame, out);
        };
        let span = |spans: &[u32], atom: usize| {
            spans[frame + atom] as usize..spans[frame + atom + 1] as usize
        };
        let here = Level::Attr(depth as u32);
        let reads_here = |scratch: &JoinScratch, atom: usize| {
            let first = scratch.cursors[span(&scratch.spans, atom).start];
            self.join.atoms[atom].levels.get(first.node.depth() - 1) == Some(&here)
        };
        // An atom read through several tries allows the union of their cells.
        let mut unions: SmallVec<[Vec<u32>; 4]> = SmallVec::new();
        for atom in 0..width {
            let cursors = span(&scratch.spans, atom);
            if cursors.len() > 1 && reads_here(scratch, atom) {
                let mut union = scratch.unions.pop().unwrap_or_default();
                union.clear();
                for cursor in &scratch.cursors[cursors] {
                    self.sources[cursor.source as usize]
                        .trie
                        .set(cursor.node)
                        .list(&mut union);
                }
                union.sort_unstable();
                union.dedup();
                unions.push(union);
            }
        }
        let mut candidates = std::mem::take(&mut scratch.candidates[depth]);
        {
            let mut merged = unions.iter();
            let mut sets: SmallVec<[Set<'_>; 8]> = SmallVec::new();
            for atom in 0..width {
                let cursors = span(&scratch.spans, atom);
                if !reads_here(scratch, atom) {
                    continue;
                }
                sets.push(match cursors.len() {
                    1 => {
                        let cursor = scratch.cursors[cursors.start];
                        self.sources[cursor.source as usize].trie.set(cursor.node)
                    }
                    _ => Set::Uint(merged.next().expect("a merged list")),
                });
            }
            Set::intersect(
                &mut sets,
                &mut candidates,
                &mut scratch.words,
                &mut scratch.spare,
            );
        }
        scratch.unions.extend(unions);
        let next = frame + width + 1;
        'cells: for &cell in &candidates {
            if allowed.is_some_and(|allowed| !allowed(var, cell)) {
                continue;
            }
            let end = scratch.spans[frame + width] as usize;
            scratch.cursors.truncate(end);
            scratch.spans.truncate(next);
            for atom in 0..width {
                scratch.spans.push(scratch.cursors.len() as u32);
                let cursors = span(&scratch.spans, atom);
                if !reads_here(scratch, atom) {
                    scratch.cursors.extend_from_within(cursors);
                    continue;
                }
                let levels = &self.join.atoms[atom].levels;
                for at in cursors {
                    let Cursor { source, mut node } = scratch.cursors[at];
                    let trie = self.sources[source as usize].trie;
                    // A variable an atom reads twice is a level per column.
                    while levels.get(node.depth() - 1) == Some(&here) {
                        match trie.child(node, cell) {
                            Some(child) => node = child,
                            None => break,
                        }
                    }
                    if levels.get(node.depth() - 1) != Some(&here) {
                        scratch.cursors.push(Cursor { source, node });
                    }
                }
                if scratch.cursors.len() as u32 == scratch.spans[next + atom] {
                    continue 'cells;
                }
            }
            scratch.spans.push(scratch.cursors.len() as u32);
            self.descend(scratch, allowed, depth + 1, out);
        }
        scratch.candidates[depth] = candidates;
    }

    /// Every atom is at its leaves: the product of the rows they hold, each
    /// as a tuple of ranks.
    fn leaves(&self, scratch: &mut JoinScratch, frame: usize, out: &mut Vec<u64>) {
        let width = self.join.atoms.len();
        for atom in 0..width {
            let list = &mut scratch.leaves[atom];
            list.clear();
            let cursors =
                scratch.spans[frame + atom] as usize..scratch.spans[frame + atom + 1] as usize;
            for cursor in &scratch.cursors[cursors] {
                let source = &self.sources[cursor.source as usize];
                let stamps = source.table.stamps();
                for &row in source.trie.rows(cursor.node) {
                    if source.old && stamps[row as usize] + 1 == self.epoch {
                        continue;
                    }
                    list.push(source.base | (row as u64) << source.shift);
                }
            }
            if list.is_empty() {
                return;
            }
        }
        let leaves = &scratch.leaves[..width];
        let mut at: SmallVec<[usize; 8]> = SmallVec::from_elem(0, width);
        loop {
            out.extend(leaves.iter().zip(&at).map(|(list, &at)| list[at]));
            let mut atom = width;
            loop {
                if atom == 0 {
                    return;
                }
                atom -= 1;
                at[atom] += 1;
                if at[atom] < leaves[atom].len() {
                    break;
                }
                at[atom] = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use smallvec::{SmallVec, smallvec};

    use super::super::ir::{Atom, Guard, Host, Param, Query, Rows, Select, Swap, Term};
    use super::super::{Matches, PlanCache};
    use super::*;
    use crate::db::SpaceId;

    const IDS: u32 = 24;
    /// Two selections over the binary tables, one over the unary table, and
    /// the cells of the binary tables whose rows also read swapped.
    const BINARY: Param = 0;
    const OTHER: Param = 1;
    const UNARY: Param = 2;
    const SWAP: Param = 3;

    struct Selections;

    impl Host for Selections {
        fn version(&self) -> u64 {
            1
        }

        fn resolve(&self, param: Param, since: Option<u64>, out: &mut Resolution) {
            if since.is_some() {
                return;
            }
            let tables: &[(u32, &[u32])] = match param {
                BINARY => &[(1, &[0, 2]), (0, &[0, 1])],
                OTHER => &[(0, &[1, 2]), (1, &[1])],
                UNARY => &[(2, &[0, 1])],
                _ => &[(1, &[0, 1]), (0, &[1])],
            };
            out.tables = tables
                .iter()
                .map(|&(table, cells)| (table, SmallVec::from_slice(cells)))
                .collect();
        }

        fn call(&self, _: u32, _: &[u64], _: &mut [u64]) -> bool {
            false
        }
    }

    type Db = Database<(), 1>;

    /// A bag and a keyed table of binary rows and a bag of unary ones, each
    /// row `(label, args..) -> value` with a few labels and few argument
    /// cells, filled over two epochs.
    fn database(rows: &[(u32, u32, u32, u32, u32)], split: usize) -> Db {
        let space = SpaceId::nth(0);
        let mut db = Db::new();
        for _ in 0..IDS {
            db.mint(space);
        }
        db.add_table(Table::bag(3).plain(1), space);
        db.add_table(Table::new(3).plain(1), space);
        db.add_table(Table::bag(2).plain(1), space);
        for (at, &(table, label, x, y, value)) in rows.iter().enumerate() {
            if at == split {
                db.advance();
            }
            let tag = at as u32;
            match table % 3 {
                2 => db.insert(2, &[label, x], value, tag),
                1 if db.table(1).unwrap().get(&[label, x, y]).is_some() => continue,
                table => db.insert(table, &[label, x, y], value, tag),
            };
        }
        db.advance();
        db
    }

    fn atom(param: Param, args: &[u32], value: u32, tag: Option<u32>, swap: bool) -> Atom {
        let mut terms: SmallVec<[Term; 6]> = smallvec![Term::Any];
        terms.extend(args.iter().map(|&arg| Term::Var(arg)));
        terms.push(Term::Var(value));
        Atom::Rows(Rows {
            select: Select { column: 0, param },
            terms,
            tag,
            swap: (swap && args.len() == 2).then_some(Swap {
                columns: [1, 2],
                when: SWAP,
            }),
        })
    }

    /// Queries with a variable two atoms share and neither has bound when
    /// they are reached: a chain, a triangle, a triangle whose second atom
    /// reads one variable twice, and a triangle reached sideways.
    fn query(shape: usize, swap: bool, guard: bool) -> Query {
        let (r, a, b, x, y, c, d) = (0, 1, 2, 3, 4, 5, 6);
        let atoms = match shape {
            0 => vec![
                atom(BINARY, &[a, b], r, Some(0), swap),
                atom(UNARY, &[x], a, Some(1), swap),
                atom(UNARY, &[x], b, None, swap),
            ],
            1 => vec![
                atom(BINARY, &[a, b], r, Some(0), swap),
                atom(OTHER, &[x, y], a, Some(1), swap),
                atom(BINARY, &[x, y], b, None, swap),
            ],
            2 => vec![
                atom(BINARY, &[a, b], r, Some(0), swap),
                atom(OTHER, &[x, x], a, Some(1), swap),
                atom(BINARY, &[x, y], b, None, swap),
            ],
            _ => vec![
                atom(BINARY, &[a, b], r, Some(0), swap),
                atom(OTHER, &[a, x], c, Some(1), swap),
                atom(BINARY, &[b, x], d, None, swap),
            ],
        };
        Query {
            space: SpaceId::nth(0),
            vars: 7,
            scalars: 2,
            params: 4,
            root: r,
            atoms,
            // Placed as soon as the root's atom binds both, which splits the
            // run there.
            guards: match guard {
                true => vec![Guard::Distinct(smallvec![(a, b)])],
                false => Vec::new(),
            },
            nots: Vec::new(),
        }
    }

    fn listed(matches: &Matches) -> Vec<(u32, Vec<Option<u32>>, Vec<u64>)> {
        (0..matches.len())
            .map(|at| {
                let (root, bindings, scalars) = matches.get(at);
                (root, bindings.to_vec(), scalars.to_vec())
            })
            .collect()
    }

    proptest! {
        /// The join emits exactly the loop nest's matches in the loop nest's
        /// order, from the root, from every row and from the new rows, under
        /// a pruning callback and with old rows required below an atom.
        #[test]
        fn a_join_matches_the_loop_nest_in_order(
            rows in prop::collection::vec((0u32..3, 0u32..3, 0u32..8, 0u32..8, 0u32..IDS), 100..400),
            split in 0usize..400,
            shape in 0usize..4,
            swap in any::<bool>(),
            guard in any::<bool>(),
            prune in 0u32..4,
            old_below in 0usize..4,
        ) {
            let db = database(&rows, split.min(rows.len() - 20));
            let plan = Plan::compile(query(shape, swap, guard));
            prop_assert!(plan.joins.len() == 1, "the query has a join: {:?}", plan.run);
            let allowed = |var: u32, id: u32| prune == 0 || !(var * 7 + id).is_multiple_of(prune + 3);
            let search = |join_min_rows| {
                let mut cache = PlanCache::default();
                cache.join_min_rows = join_min_rows;
                let roots = plan.search_roots(&db, &Selections, 0..IDS, Some(&allowed), (false, 0), &mut cache);
                let fresh = plan.search_roots(&db, &Selections, 0..IDS, Some(&allowed), (true, old_below), &mut cache);
                let all = plan.search_all(&db, &Selections, &mut cache);
                let new = plan.search_new(&db, &Selections, &mut cache);
                [roots, fresh, all, new].iter().map(listed).collect::<Vec<_>>()
            };
            prop_assert_eq!(search(0), search(usize::MAX));
        }
    }
}
