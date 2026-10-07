//! The generic relational layer: tables of `u32` cells with nothing in them
//! that knows what an e-graph is.
//!
//! A [`Table`] is a relation with a functional dependency: its key columns
//! determine its value column, and a hash index over the key enforces that. An
//! edge list is a table (`(source, label) -> target`), and so is an operator of
//! an e-graph (`children -> class`). What a client does when two rows come to
//! share a key is the client's business; the table reports the pair.
//!
//! Each row also carries a tag, a word the table stores and never interprets,
//! and a stamp, the epoch the row was last written in, which is what lets a
//! fixpoint evaluation join only against what changed.
//!
//! The loops over whole columns are [`tir_adt::simd`] kernels.

use tir_adt::simd::{self, hash_row};

/// An index places the rows appended or moved since its base was built until
/// they, with the base entries they left behind, outnumber this fraction of
/// the base, and then rebuilds the base.
pub(crate) const REBUILD_DIVISOR: usize = 8;

/// Slot of the key index that never held a row.
const EMPTY: u32 = 0;
/// Slot whose row was removed; a probe walks past it.
const TOMBSTONE: u32 = u32::MAX;

/// Two rows [`Table::repair`] found under one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Collision {
    /// The value of the row that stays.
    pub kept: u32,
    /// The value and tag of the row that was removed.
    pub removed: u32,
    pub tag: u32,
}

/// What a [`Table::repair`] did, appended to whatever the lists already hold.
#[derive(Clone, Debug, Default)]
pub struct Repair {
    pub collisions: Vec<Collision>,
    /// The value of every row whose key moved, removed rows included.
    pub rekeyed: Vec<u32>,
}

/// A relation whose first `arity` columns determine the last.
#[derive(Clone, Debug)]
pub struct Table {
    arity: usize,
    /// `arity + 1` columns of equal length; the last is the value.
    columns: Vec<Vec<u32>>,
    /// How many leading key columns [`Self::repair`] leaves as they are: cells
    /// that name something other than what the map renames.
    plain: usize,
    tag: Vec<u32>,
    stamp: Vec<u32>,
    /// Whether the key is unique. A table that is not keyed is a bag: it has no
    /// index and never reports a collision.
    keyed: bool,
    /// Open addressing over the key columns: `row + 1`, [`EMPTY`] or
    /// [`TOMBSTONE`] in the low half, the key's hash in the high half so a
    /// probe reads the columns only for a row that can match. Its length is a
    /// power of two.
    slots: Vec<u64>,
    /// Slots that are not [`EMPTY`].
    used: usize,
    /// Bumped by every change, so a reader can tell a derived index is stale.
    version: u64,
    /// The rows every [`Self::repair`] rewrote or moved another row into,
    /// oldest first, with per repair the version it left the table at, where
    /// its rows start and the length it left the table at; and the version
    /// the log is complete from. The log is what lets an index merge a repair
    /// instead of rebuilding.
    touched: Vec<u32>,
    repairs: Vec<(u64, u32, u32)>,
    floor: u64,
    key: Vec<u32>,
    flags: Vec<u32>,
    moved: Vec<u32>,
    dead: Vec<u32>,
}

impl Table {
    pub fn new(arity: usize) -> Self {
        Self {
            arity,
            columns: vec![Vec::new(); arity + 1],
            plain: 0,
            tag: Vec::new(),
            stamp: Vec::new(),
            keyed: true,
            slots: vec![EMPTY as u64; 8],
            used: 0,
            version: 0,
            touched: Vec::new(),
            repairs: Vec::new(),
            floor: 0,
            key: vec![0; arity],
            flags: Vec::new(),
            moved: Vec::new(),
            dead: Vec::new(),
        }
    }

    /// A table whose rows may repeat a key.
    pub fn bag(arity: usize) -> Self {
        Self {
            keyed: false,
            slots: Vec::new(),
            ..Self::new(arity)
        }
    }

    /// Leave the first `columns` key columns out of [`Self::repair`]'s rewrite.
    pub fn plain(mut self, columns: usize) -> Self {
        self.plain = columns;
        self
    }

    pub fn arity(&self) -> usize {
        self.arity
    }

    /// How many leading key columns hold cells [`Self::repair`] leaves alone.
    pub fn plain_columns(&self) -> usize {
        self.plain
    }

    pub fn len(&self) -> usize {
        self.stamp.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stamp.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The rows the repairs since the table was at `version` rewrote or
    /// moved another row into, and the fewest rows the table has had since;
    /// `None` when the table no longer remembers that far back.
    pub fn moved_since(&self, version: u64) -> Option<(&[u32], usize)> {
        if version < self.floor {
            return None;
        }
        let at = self
            .repairs
            .partition_point(|&(after, ..)| after <= version);
        let since = &self.repairs[at..];
        let start = since
            .first()
            .map_or(self.touched.len(), |&(_, start, _)| start as usize);
        let shortest = since.iter().map(|&(.., len)| len as usize).min();
        Some((&self.touched[start..], shortest.unwrap_or(self.len())))
    }

    /// One column, key columns first and the value column last.
    pub fn column(&self, index: usize) -> &[u32] {
        &self.columns[index]
    }

    pub fn values(&self) -> &[u32] {
        &self.columns[self.arity]
    }

    pub fn tags(&self) -> &[u32] {
        &self.tag
    }

    /// The epoch each row was last written in.
    pub fn stamps(&self) -> &[u32] {
        &self.stamp
    }

    fn row_matches(&self, row: usize, key: &[u32]) -> bool {
        self.columns
            .iter()
            .zip(key)
            .all(|(column, &cell)| column[row] == cell)
    }

    /// The row holding `key`, if any. Always `None` for a bag.
    pub fn get(&self, key: &[u32]) -> Option<u32> {
        debug_assert_eq!(key.len(), self.arity);
        if !self.keyed {
            return None;
        }
        let hash = hash_row(key);
        let mask = self.slots.len() - 1;
        let mut at = hash as usize & mask;
        loop {
            let slot = self.slots[at];
            match slot as u32 {
                EMPTY => return None,
                TOMBSTONE => {}
                row if (slot >> 32) as u32 == hash && self.row_matches(row as usize - 1, key) => {
                    return Some(row - 1);
                }
                _ => {}
            }
            at = (at + 1) & mask;
        }
    }

    /// Append `key -> value`. In a keyed table the key must not be present.
    pub fn insert(&mut self, key: &[u32], value: u32, tag: u32, stamp: u32) -> u32 {
        debug_assert!(self.get(key).is_none(), "the key is new");
        if self.keyed {
            self.reserve(1);
        }
        let row = self.len() as u32;
        for (column, &cell) in self.columns.iter_mut().zip(key) {
            column.push(cell);
        }
        self.columns[self.arity].push(value);
        self.tag.push(tag);
        self.stamp.push(stamp);
        if self.keyed {
            self.place(row, hash_row(key));
        }
        self.version += 1;
        row
    }

    /// Put `row` in the first free slot of its probe sequence.
    fn place(&mut self, row: u32, hash: u32) {
        let mask = self.slots.len() - 1;
        let mut at = hash as usize & mask;
        while self.slots[at] as u32 != EMPTY && self.slots[at] as u32 != TOMBSTONE {
            at = (at + 1) & mask;
        }
        self.used += usize::from(self.slots[at] as u32 == EMPTY);
        self.slots[at] = (hash as u64) << 32 | (row + 1) as u64;
    }

    /// Make room for `extra` more entries with the index at most half full,
    /// which is what keeps a probe finite.
    fn reserve(&mut self, extra: usize) {
        if (self.used + extra) * 2 <= self.slots.len() {
            return;
        }
        let mut hashes = vec![0; self.len()];
        let keys: Vec<&[u32]> = self.columns[..self.arity]
            .iter()
            .map(Vec::as_slice)
            .collect();
        simd::hash_rows(&keys, &mut hashes);
        let size = ((self.len() + extra) * 4).next_power_of_two().max(8);
        self.slots.clear();
        self.slots.resize(size, EMPTY as u64);
        self.used = 0;
        // Rows in `moved` are out of the index until their caller puts them
        // back; the list is ascending.
        let mut moved = 0;
        for (row, hash) in hashes.into_iter().enumerate() {
            if self.moved.get(moved) == Some(&(row as u32)) {
                moved += 1;
                continue;
            }
            self.place(row as u32, hash);
        }
    }

    /// The slot that holds `row`, whose key is loaded.
    fn slot_of(&self, row: u32) -> usize {
        let mask = self.slots.len() - 1;
        let mut at = hash_row(&self.key) as usize & mask;
        while self.slots[at] as u32 != row + 1 {
            at = (at + 1) & mask;
        }
        at
    }

    fn load_key(&mut self, row: u32) {
        for (cell, column) in self.key.iter_mut().zip(&self.columns) {
            *cell = column[row as usize];
        }
    }

    /// The rows of `column` that `map` moves, ascending, into `self.moved`.
    fn select_moved(&mut self, columns: std::ops::Range<usize>, map: &[u32]) {
        self.flags.clear();
        self.flags.resize(self.len(), 0);
        for column in columns {
            simd::mark_moved(&self.columns[column], map, &mut self.flags);
        }
        self.moved.clear();
        simd::select_eq(&self.flags, 1, &mut self.moved);
    }

    /// Rewrite every key cell past the plain ones, and every value cell, through
    /// `map`, and restore the functional dependency.
    ///
    /// `map` sends a cell to the cell that now stands for it, and a cell that
    /// stands for itself to itself. A row any of whose cells moved is stamped
    /// `stamp`. Where two rows end up with one key, the later one is removed
    /// and the pair is reported for the caller to reconcile. Row numbers are
    /// not stable across this call.
    ///
    /// Reports whether any cell moved.
    pub fn repair(&mut self, map: &[u32], stamp: u32, report: &mut Repair) -> bool {
        let start = self.touched.len();
        self.select_moved(self.arity..self.arity + 1, map);
        let mut any = !self.moved.is_empty();
        for at in 0..self.moved.len() {
            let row = self.moved[at] as usize;
            self.columns[self.arity][row] = map[self.columns[self.arity][row] as usize];
            self.stamp[row] = stamp;
        }
        self.touched.extend_from_slice(&self.moved);
        let mut logged = start..self.touched.len();
        self.select_moved(self.plain..self.arity, map);
        any |= !self.moved.is_empty();
        for at in 0..self.moved.len() {
            let row = self.moved[at];
            if self.keyed {
                // The index is keyed on the cells as they were, so the entry
                // goes before they change.
                self.load_key(row);
                let slot = self.slot_of(row);
                self.slots[slot] = TOMBSTONE as u64;
            }
            for column in &mut self.columns[self.plain..self.arity] {
                column[row as usize] = map[column[row as usize] as usize];
            }
            self.stamp[row as usize] = stamp;
            report.rekeyed.push(self.columns[self.arity][row as usize]);
        }
        if self.keyed {
            self.reserve(self.moved.len());
            for at in 0..self.moved.len() {
                let row = self.moved[at];
                self.load_key(row);
                let key = std::mem::take(&mut self.key);
                match self.get(&key) {
                    Some(kept) => {
                        report.collisions.push(Collision {
                            kept: self.columns[self.arity][kept as usize],
                            removed: self.columns[self.arity][row as usize],
                            tag: self.tag[row as usize],
                        });
                        self.dead.push(row);
                    }
                    None => {
                        self.place(row, hash_row(&key));
                        self.touch(row, &mut logged);
                    }
                }
                self.key = key;
            }
        } else {
            for at in 0..self.moved.len() {
                self.touch(self.moved[at], &mut logged);
            }
        }
        self.moved.clear();
        // Highest first, so the row that fills a hole is never one still to go.
        for at in (0..self.dead.len()).rev() {
            self.swap_remove(self.dead[at]);
        }
        self.dead.clear();
        if any {
            self.version += 1;
            self.repairs
                .push((self.version, start as u32, self.len() as u32));
            // A log longer than the delta an index rebuilds at serves no one.
            if self.touched.len() > self.len() / REBUILD_DIVISOR {
                self.touched.clear();
                self.repairs.clear();
                self.floor = self.version;
            }
        }
        any
    }

    /// Log `row` as touched unless the value phase did, whose rows `logged`
    /// walks: both ascend.
    fn touch(&mut self, row: u32, logged: &mut std::ops::Range<usize>) {
        while logged.start < logged.end && self.touched[logged.start] < row {
            logged.start += 1;
        }
        if logged.start == logged.end || self.touched[logged.start] != row {
            self.touched.push(row);
        }
    }

    /// Remove `row`, which the index does not hold, moving the last row into
    /// its place.
    fn swap_remove(&mut self, row: u32) {
        let last = self.len() as u32 - 1;
        if row != last {
            self.load_key(last);
            let slot = self.slot_of(last);
            self.slots[slot] = self.slots[slot] >> 32 << 32 | (row + 1) as u64;
            self.touched.push(row);
        }
        for column in &mut self.columns {
            column.swap_remove(row as usize);
        }
        self.tag.swap_remove(row as usize);
        self.stamp.swap_remove(row as usize);
    }
}

/// What an index keeps beside its base, built over the first rows of a
/// table: which rows changed since, for the index to place again, and how
/// much it has placed and dropped, which decides when the base is rebuilt.
#[derive(Clone, Debug, Default)]
pub(crate) struct Delta {
    /// The rows the last refresh found changed, ascending, and the rows the
    /// table dropped since the one before.
    pub(crate) changed: Vec<u32>,
    pub(crate) dropped: std::ops::Range<u32>,
    /// The table's version and length at the last refresh.
    version: u64,
    seen: usize,
    built: bool,
    /// How many rows the base was built over, and how many were placed or
    /// dropped since.
    base: usize,
    merged: usize,
}

/// What [`Delta::refresh`] found to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// The table is as it was.
    Current,
    /// The changed rows are to leave where they sit and be placed again under
    /// their cells now, and the dropped rows to leave.
    Merged,
    /// The base is to be built over the table again.
    Rebuild,
}

/// Where a row sits in an index when not at a position of its base.
pub(crate) const ABSENT: u32 = u32::MAX;
pub(crate) const TAIL: u32 = u32::MAX - 1;

impl Delta {
    /// Bring the delta up to date with `table`.
    pub(crate) fn refresh(&mut self, table: &Table) -> Refresh {
        let len = table.len();
        if self.built && self.version == table.version() {
            return Refresh::Current;
        }
        let moved = self
            .built
            .then(|| table.moved_since(self.version))
            .flatten();
        self.version = table.version();
        let seen = std::mem::replace(&mut self.seen, len);
        if let Some((touched, shortest)) = moved {
            // The rows repairs touched and the rows appended since, each
            // once.
            self.changed.clear();
            self.changed
                .extend(touched.iter().filter(|&&row| (row as usize) < len));
            self.changed.extend(shortest.min(seen) as u32..len as u32);
            self.changed.sort_unstable();
            self.changed.dedup();
            self.dropped = len as u32..seen.max(len) as u32;
            self.merged += self.changed.len() + self.dropped.len();
            if self.merged <= self.base / REBUILD_DIVISOR {
                return Refresh::Merged;
            }
        }
        Refresh::Rebuild
    }

    /// Note the base built over `base` rows.
    pub(crate) fn rebuilt(&mut self, base: usize) {
        self.built = true;
        self.base = base;
        self.merged = 0;
        self.changed.clear();
        self.dropped = 0..0;
    }

    /// The rows that leave where they sit: the changed and the dropped.
    pub(crate) fn leaving(&self) -> impl Iterator<Item = u32> + '_ {
        self.changed.iter().copied().chain(self.dropped.clone())
    }
}

/// Take `row` out of a base whose entries lie in the ascending runs
/// `rows[from..to]` of `runs`, `pos` saying where each row sits.
pub(crate) fn drop_row(rows: &mut [u32], pos: &mut [u32], runs: &mut [(u32, u32)], row: u32) {
    let at = pos[row as usize];
    let run = runs.partition_point(|&(from, _)| from <= at) - 1;
    let (at, end) = (at as usize, runs[run].1 as usize);
    pos[row as usize] = ABSENT;
    rows.copy_within(at + 1..end, at);
    for &row in &rows[at..end - 1] {
        pos[row as usize] -= 1;
    }
    runs[run].1 -= 1;
}

/// The rows of a table grouped by one column: `cell -> rows`, for the side of a
/// join that is looked up rather than scanned. Cells are dense small numbers,
/// so the grouping is a counting sort and a lookup is a slice.
///
/// The counting sort is a base over the rows the table had when it was built,
/// each cell's run left some slack. A row that changes since is placed in its
/// cell's run, or in a tail sorted by cell when the cell has no run; a run
/// that fills up has the base rebuilt.
#[derive(Clone, Debug, Default)]
pub struct Group {
    /// `rows[from..to]` is the cell's run for `ranges[cell] == (from, to)`,
    /// which may grow to the next cell's `from`; the last entry closes the
    /// last run. `pos[row]` is where a row sits, [`TAIL`] or [`ABSENT`].
    ranges: Vec<(u32, u32)>,
    rows: Vec<u32>,
    pos: Vec<u32>,
    live: usize,
    delta: Delta,
    /// The tail's rows sorted by cell, then ascending, their cells, and a bit
    /// per cell that has any.
    tail: Vec<u32>,
    cells: Vec<u32>,
    has: Vec<u64>,
}

impl Group {
    /// Bring the grouping of `table` by `column` up to date. Cells of a column
    /// past the plain ones are below `ids`.
    pub fn refresh(&mut self, table: &Table, column: usize, ids: usize) {
        let cells = table.column(column);
        match self.delta.refresh(table) {
            Refresh::Current => return,
            Refresh::Merged if self.place(cells) => return,
            _ => {}
        }
        let bound = match column < table.plain_columns() {
            true => cells.iter().max().map_or(0, |&max| max as usize + 1),
            false => ids,
        };
        self.ranges.clear();
        self.ranges.resize(bound + 1, (0, 0));
        for &cell in cells {
            self.ranges[cell as usize].1 += 1;
        }
        // Each run gets an eighth and one of slack.
        let mut sum = 0;
        for range in &mut self.ranges {
            let count = range.1;
            *range = (sum, sum);
            sum += count + if count == 0 { 0 } else { count / 8 + 1 };
        }
        self.rows.clear();
        self.rows.resize(sum as usize, 0);
        self.pos.clear();
        self.pos.resize(cells.len(), ABSENT);
        // A cell's end is its cursor, and ends as its end.
        for (row, &cell) in cells.iter().enumerate() {
            let cursor = &mut self.ranges[cell as usize].1;
            self.rows[*cursor as usize] = row as u32;
            self.pos[row] = *cursor;
            *cursor += 1;
        }
        self.live = cells.len();
        self.tail.clear();
        self.cells.clear();
        self.has.clear();
        self.delta.rebuilt(cells.len());
    }

    /// Place the changed rows again; `false` when a run is full, which has
    /// the base rebuilt.
    fn place(&mut self, cells: &[u32]) -> bool {
        if self.pos.len() < cells.len() {
            self.pos.resize(cells.len(), ABSENT);
        }
        for row in self.delta.leaving() {
            match self.pos[row as usize] {
                ABSENT => {}
                TAIL => self.pos[row as usize] = ABSENT,
                _ => {
                    drop_row(&mut self.rows, &mut self.pos, &mut self.ranges, row);
                    self.live -= 1;
                }
            }
        }
        let pos = &self.pos;
        self.tail.retain(|&row| pos[row as usize] == TAIL);
        for &row in &self.delta.changed {
            let cell = cells[row as usize] as usize;
            let (from, to) = self.ranges.get(cell).copied().unwrap_or((0, 0));
            let room = self.ranges.get(cell + 1).map_or(0, |next| next.0);
            if to < room {
                let run = &self.rows[from as usize..to as usize];
                let at = from as usize + run.partition_point(|&other| other < row);
                self.rows.copy_within(at..to as usize, at + 1);
                self.rows[at] = row;
                for &moved in &self.rows[at + 1..=to as usize] {
                    self.pos[moved as usize] += 1;
                }
                self.pos[row as usize] = at as u32;
                self.ranges[cell].1 += 1;
                self.live += 1;
            } else if from != to {
                return false;
            } else {
                self.tail.push(row);
                self.pos[row as usize] = TAIL;
            }
        }
        self.tail
            .sort_unstable_by_key(|&row| (cells[row as usize], row));
        self.cells.clear();
        self.cells
            .extend(self.tail.iter().map(|&row| cells[row as usize]));
        self.has.clear();
        let top = self.cells.last().map_or(0, |&cell| cell as usize + 1);
        self.has.resize(top.div_ceil(64), 0);
        for &cell in &self.cells {
            self.has[cell as usize / 64] |= 1 << (cell % 64);
        }
        true
    }

    /// The rows holding `cell`, ascending.
    #[inline(always)]
    pub fn rows(&self, cell: u32) -> &[u32] {
        match self.ranges.get(cell as usize) {
            Some(&(from, to)) if from != to => &self.rows[from as usize..to as usize],
            _ => match self.has.get(cell as usize / 64) {
                Some(word) if word >> (cell % 64) & 1 != 0 => self.tail_of(cell),
                _ => &[],
            },
        }
    }

    #[inline(never)]
    fn tail_of(&self, cell: u32) -> &[u32] {
        let from = self.cells.partition_point(|&other| other < cell);
        let to = from + self.cells[from..].partition_point(|&other| other == cell);
        &self.tail[from..to]
    }

    /// How many cells the grouping has room for: one past the highest.
    pub fn cells(&self) -> usize {
        let tail = self.cells.last().map_or(0, |&cell| cell as usize + 1);
        self.ranges.len().saturating_sub(1).max(tail)
    }

    /// How many rows the cells have, all together.
    pub fn len(&self) -> usize {
        self.live + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_key_is_found_after_the_index_grows() {
        let mut table = Table::new(2);
        for i in 0..100 {
            table.insert(&[i, i + 1], i * 10, i, 1);
        }
        assert_eq!(table.get(&[7, 8]), Some(7));
        assert_eq!(table.get(&[7, 9]), None);
        assert_eq!(table.values()[7], 70);
    }

    #[test]
    fn a_table_with_no_key_columns_holds_one_row() {
        let mut table = Table::new(0);
        assert_eq!(table.get(&[]), None);
        table.insert(&[], 5, 0, 1);
        assert_eq!(table.get(&[]), Some(0));
    }

    #[test]
    fn repair_reports_rows_that_come_to_share_a_key() {
        let mut table = Table::new(1);
        table.insert(&[0], 10, 100, 1);
        table.insert(&[1], 11, 101, 1);
        table.insert(&[2], 12, 102, 1);
        // Cell 1 now stands for cell 0, so the first two rows collide.
        let map: Vec<u32> = (0..13)
            .map(|cell| if cell == 1 { 0 } else { cell })
            .collect();
        let mut report = Repair::default();
        assert!(table.repair(&map, 2, &mut report));
        assert_eq!(
            report.collisions,
            vec![Collision {
                kept: 10,
                removed: 11,
                tag: 101
            }]
        );
        assert_eq!(report.rekeyed, vec![11]);
        assert_eq!(table.len(), 2);
        let value = |key: u32| table.get(&[key]).map(|row| table.values()[row as usize]);
        assert_eq!((value(0), value(1), value(2)), (Some(10), None, Some(12)));
    }

    #[test]
    fn a_bag_keeps_rows_that_come_to_share_a_key() {
        let mut table = Table::bag(1);
        table.insert(&[0], 10, 0, 1);
        table.insert(&[1], 11, 1, 1);
        let mut report = Repair::default();
        assert!(table.repair(&[0, 0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11], 2, &mut report));
        assert!(report.collisions.is_empty());
        assert_eq!(table.column(0), &[0, 0]);
        assert_eq!(table.stamps(), &[1, 2]);
    }

    proptest! {
        /// Repair against a model: the surviving keys are the mapped keys, each
        /// once, and every one is reachable through the index.
        #[test]
        fn repair_leaves_one_row_per_mapped_key(
            keys in prop::collection::btree_set((0u32..12, 0u32..12), 0..60),
            map in prop::collection::vec(0u32..12, 12),
        ) {
            // Make the map idempotent, as a flattened union-find is.
            let map: Vec<u32> = (0..12).map(|cell| {
                let mut at = cell;
                for _ in 0..12 { at = at.min(map[at as usize]); }
                at
            }).collect();
            let map: Vec<u32> = map.iter().map(|&cell| map[cell as usize]).collect();
            prop_assume!(map.iter().all(|&cell| map[cell as usize] == cell));
            let mut table = Table::new(2);
            for (tag, &(a, b)) in keys.iter().enumerate() {
                table.insert(&[a, b], 0, tag as u32, 1);
            }
            let mut report = Repair::default();
            table.repair(&map, 2, &mut report);
            let expected: std::collections::BTreeSet<(u32, u32)> =
                keys.iter().map(|&(a, b)| (map[a as usize], map[b as usize])).collect();
            let found: std::collections::BTreeSet<(u32, u32)> =
                (0..table.len()).map(|row| (table.column(0)[row], table.column(1)[row])).collect();
            prop_assert_eq!(table.len(), expected.len());
            prop_assert_eq!(report.collisions.len(), keys.len() - expected.len());
            for &(a, b) in &expected {
                let row = table.get(&[a, b]);
                prop_assert!(row.is_some());
                prop_assert_eq!((table.column(0)[row.unwrap() as usize], table.column(1)[row.unwrap() as usize]), (a, b));
            }
            prop_assert_eq!(found, expected);
        }

        /// However the table grew and was repaired between refreshes, a
        /// grouping lists each cell's rows ascending.
        #[test]
        fn a_group_lists_each_cells_rows_ascending(
            rows in prop::collection::vec((0u32..32, 0u32..32, 0u32..32), 0..300),
            cuts in prop::collection::vec((0usize..300, prop::option::of(1u32..32)), 0..8),
            keyed in any::<bool>(),
            column in 0usize..3,
        ) {
            let mut table = if keyed { Table::new(2) } else { Table::bag(2) };
            let mut group = Group::default();
            let mut cuts: Vec<_> = cuts.into_iter().map(|(cut, repair)| (cut.min(rows.len()), repair)).collect();
            cuts.sort();
            cuts.push((rows.len(), None));
            let mut inserted = 0;
            for (cut, moved) in cuts {
                for &(a, b, value) in &rows[inserted..cut] {
                    if !keyed || table.get(&[a, b]).is_none() {
                        table.insert(&[a, b], value, 0, 1);
                    }
                }
                inserted = cut;
                // One cell comes to stand for the one below it.
                if let Some(moved) = moved {
                    let map: Vec<u32> = (0..32).map(|cell| if cell == moved { moved - 1 } else { cell }).collect();
                    table.repair(&map, 2, &mut Repair::default());
                }
                group.refresh(&table, column, 32);
                let cells = table.column(column);
                for cell in 0..34 {
                    let expected: Vec<u32> = (0..table.len() as u32).filter(|&row| cells[row as usize] == cell).collect();
                    prop_assert_eq!(group.rows(cell), &expected[..]);
                }
            }
        }
    }
}
