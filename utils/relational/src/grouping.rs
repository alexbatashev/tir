//! The rows of one partition grouped by a cell they hold: a sorted base built
//! over the partition as it stood, and a sorted tail of the rows written
//! since.
//!
//! An owner row contributes entries `(cell, index)`: an operation contributes
//! one per operand, an operation in a block contributes its block and its
//! position. A lookup answers the entries holding one cell. [`Grouping`] is
//! for owners holding several cells, [`Lists`] for owners holding one. A base goes
//! stale one owner at a time: an owner stamped after the base was built is read
//! from the tail instead, which holds that owner's entries as they stand. The
//! tail is brought up to date from the partition's write log when a lookup
//! asks, so a run of writes with no lookup between them costs nothing here,
//! and the base is rebuilt once the tail outgrows a fraction of it.

use std::sync::Arc;

use crate::store::REBUILD_DIVISOR;

/// A tail shorter than this never has the base rebuilt.
const MIN_TAIL: usize = 64;

/// What a grouping reads its partition through.
pub trait Rows {
    /// Every live owner of the partition.
    fn owners(&self, each: &mut dyn FnMut(u32));
    /// The stamp `owner` was last written at.
    fn stamp(&self, owner: u32) -> u32;
    /// The `(cell, index)` entries `owner` contributes now; none once erased.
    fn entries(&self, owner: u32, each: &mut dyn FnMut(u32, u32));
}

/// `cell -> (owner, index)` over one partition, for owners holding several
/// cells: the use lists.
#[derive(Clone, Debug, Default)]
pub struct Grouping {
    /// `[cell, owner, index]` ascending.
    base: Arc<Vec<[u32; 3]>>,
    /// The stamp the base was built at.
    built: Option<u32>,
    tail: Vec<[u32; 3]>,
    /// How much of the write log the tail covers.
    cursor: usize,
}

impl Grouping {
    pub fn is_built(&self) -> bool {
        self.built.is_some()
    }

    /// Forget the base, so the next refresh builds it.
    pub fn reset(&mut self) {
        self.built = None;
        self.tail.clear();
    }

    /// How much of the write log this grouping has read, or `None` when it
    /// reads none of it yet.
    pub fn cursor(&self) -> Option<usize> {
        self.built.map(|_| self.cursor)
    }

    /// The log was truncated to nothing.
    pub fn rewind(&mut self) {
        self.cursor = 0;
    }

    /// Bring the grouping up to date with `rows`, whose owners written since
    /// the last call are the entries of `log` past [`Self::cursor`]. No row is
    /// stamped later than `stamp` yet, and every later write must be.
    pub fn refresh<R: Rows>(&mut self, rows: &R, log: &[u32], stamp: u32) {
        if self.built.is_none() {
            return self.rebuild(rows, log, stamp);
        }
        if self.cursor >= log.len() {
            return;
        }
        let news = news(&log[self.cursor..]);
        self.cursor = log.len();
        self.tail
            .retain(|entry| news.binary_search(&entry[1]).is_err());
        let tail = &mut self.tail;
        for owner in news {
            rows.entries(owner, &mut |cell, index| tail.push([cell, owner, index]));
        }
        if self.tail.len() > MIN_TAIL.max(self.base.len() / REBUILD_DIVISOR) {
            return self.rebuild(rows, log, stamp);
        }
        self.tail.sort_unstable();
    }

    fn rebuild<R: Rows>(&mut self, rows: &R, log: &[u32], stamp: u32) {
        let mut base = Vec::with_capacity(self.base.len() + self.tail.len());
        rows.owners(&mut |owner| {
            rows.entries(owner, &mut |cell, index| base.push([cell, owner, index]));
        });
        base.sort_unstable();
        self.base = Arc::new(base);
        self.built = Some(stamp);
        self.tail.clear();
        self.cursor = log.len();
    }

    fn range(entries: &[[u32; 3]], cell: u32) -> &[[u32; 3]] {
        let from = entries.partition_point(|entry| entry[0] < cell);
        let to = from + entries[from..].partition_point(|entry| entry[0] == cell);
        &entries[from..to]
    }

    /// Append the `(owner, index)` entries holding `cell` to `out`, in owner
    /// order with the owners written since the base was built last. The
    /// grouping must be refreshed.
    pub fn read<R: Rows>(&self, rows: &R, cell: u32, out: &mut Vec<(u32, u32)>) {
        let built = self.built.expect("a refreshed grouping");
        for entry in Self::range(&self.base, cell) {
            if rows.stamp(entry[1]) <= built {
                out.push((entry[1], entry[2]));
            }
        }
        out.extend(
            Self::range(&self.tail, cell)
                .iter()
                .map(|entry| (entry[1], entry[2])),
        );
    }

    /// Whether any entry holds `cell`.
    pub fn any<R: Rows>(&self, rows: &R, cell: u32) -> bool {
        let built = self.built.expect("a refreshed grouping");
        !Self::range(&self.tail, cell).is_empty()
            || Self::range(&self.base, cell)
                .iter()
                .any(|entry| rows.stamp(entry[1]) <= built)
    }

    /// Heap bytes the grouping holds, the shared base included.
    pub fn bytes(&self) -> usize {
        (self.base.capacity() + self.tail.capacity()) * size_of::<[u32; 3]>()
    }
}

/// The owners a stretch of a write log names, ascending, each once.
fn news(log: &[u32]) -> Vec<u32> {
    let mut news = log.to_vec();
    news.sort_unstable();
    news.dedup();
    news
}

/// `cell -> owners` over one partition, for owners holding one cell at a
/// position: the operations of each block, the blocks of each region. A list
/// reads back as one slice in position order.
///
/// The base lists every cell's owners as they stood. A cell an owner joined,
/// left or moved inside since then has its whole list kept beside the base,
/// made again whenever one of those owners is written, so a lookup never
/// checks an owner: it finds the cell among the remade lists or in the base.
#[derive(Clone, Debug, Default)]
pub struct Lists {
    /// The cells holding anything, ascending; `offsets[at]..offsets[at + 1]`
    /// of `owners` is the list of `cells[at]`.
    cells: Arc<Vec<u32>>,
    offsets: Arc<Vec<u32>>,
    owners: Arc<Vec<u32>>,
    /// `(owner, cell)` of the base, ascending by owner.
    placed: Arc<Vec<(u32, u32)>>,
    built: bool,
    cursor: usize,
    /// The owners written since the base was built, ascending, with the cell
    /// each held when it was last looked at; `u32::MAX` for none.
    moved: Vec<(u32, u32)>,
    /// The remade lists, ascending by cell.
    remade: Vec<(u32, Vec<u32>)>,
}

/// No cell: an owner in no list.
const NOWHERE: u32 = u32::MAX;

impl Lists {
    pub fn is_built(&self) -> bool {
        self.built
    }

    pub fn reset(&mut self) {
        self.built = false;
        self.moved.clear();
        self.remade.clear();
    }

    pub fn cursor(&self) -> Option<usize> {
        self.built.then_some(self.cursor)
    }

    pub fn rewind(&mut self) {
        self.cursor = 0;
    }

    /// The cell and position `owner` holds now.
    fn place<R: Rows>(rows: &R, owner: u32) -> (u32, u32) {
        let mut place = (NOWHERE, 0);
        rows.entries(owner, &mut |cell, position| place = (cell, position));
        place
    }

    /// Bring the lists up to date with `rows`; see [`Grouping::refresh`].
    pub fn refresh<R: Rows>(&mut self, rows: &R, log: &[u32]) {
        if !self.built {
            return self.rebuild(rows, log);
        }
        if self.cursor >= log.len() {
            return;
        }
        let news = news(&log[self.cursor..]);
        self.cursor = log.len();
        // The cells whose lists changed: where each written owner sat, and
        // where it sits now.
        let mut touched = Vec::with_capacity(news.len() * 2);
        for &owner in &news {
            let now = Self::place(rows, owner).0;
            let was = match self.moved.binary_search_by_key(&owner, |moved| moved.0) {
                Ok(at) => std::mem::replace(&mut self.moved[at].1, now),
                Err(at) => {
                    self.moved.insert(at, (owner, now));
                    match self.placed.binary_search_by_key(&owner, |placed| placed.0) {
                        Ok(at) => self.placed[at].1,
                        Err(_) => NOWHERE,
                    }
                }
            };
            touched.extend([was, now]);
        }
        if self.moved.len() > MIN_TAIL.max(self.owners.len() / REBUILD_DIVISOR) {
            return self.rebuild(rows, log);
        }
        touched.sort_unstable();
        touched.dedup();
        touched.retain(|cell| *cell != NOWHERE);
        // Each touched cell's list: its base owners that were not written,
        // and the written owners sitting in it now.
        let mut lists: Vec<Vec<(u32, u32)>> = vec![Vec::new(); touched.len()];
        for (list, &cell) in lists.iter_mut().zip(&touched) {
            for &owner in self.base(cell) {
                if self
                    .moved
                    .binary_search_by_key(&owner, |moved| moved.0)
                    .is_err()
                {
                    list.push((Self::place(rows, owner).1, owner));
                }
            }
        }
        for &(owner, cell) in &self.moved {
            if let Ok(at) = touched.binary_search(&cell) {
                lists[at].push((Self::place(rows, owner).1, owner));
            }
        }
        for (cell, mut list) in touched.into_iter().zip(lists) {
            list.sort_unstable();
            let list = list.into_iter().map(|(_, owner)| owner).collect();
            match self.remade.binary_search_by_key(&cell, |remade| remade.0) {
                Ok(at) => self.remade[at].1 = list,
                Err(at) => self.remade.insert(at, (cell, list)),
            }
        }
    }

    fn rebuild<R: Rows>(&mut self, rows: &R, log: &[u32]) {
        let mut entries = Vec::with_capacity(self.owners.len() + self.moved.len());
        let mut placed = Vec::with_capacity(entries.capacity());
        rows.owners(&mut |owner| {
            rows.entries(owner, &mut |cell, position| {
                entries.push((cell, position, owner));
                placed.push((owner, cell));
            });
        });
        entries.sort_unstable();
        placed.sort_unstable();
        let (mut cells, mut offsets) = (Vec::new(), Vec::new());
        for (at, entry) in entries.iter().enumerate() {
            if cells.last() != Some(&entry.0) {
                cells.push(entry.0);
                offsets.push(at as u32);
            }
        }
        offsets.push(entries.len() as u32);
        self.owners = Arc::new(entries.into_iter().map(|entry| entry.2).collect());
        self.cells = Arc::new(cells);
        self.offsets = Arc::new(offsets);
        self.placed = Arc::new(placed);
        self.built = true;
        self.cursor = log.len();
        self.moved.clear();
        self.remade.clear();
    }

    fn base(&self, cell: u32) -> &[u32] {
        match self.cells.binary_search(&cell) {
            Ok(at) => &self.owners[self.offsets[at] as usize..self.offsets[at + 1] as usize],
            Err(_) => &[],
        }
    }

    /// The owners holding `cell`, in position order. The lists must be
    /// refreshed.
    #[inline]
    pub fn read(&self, cell: u32) -> &[u32] {
        debug_assert!(self.built, "refreshed lists");
        if !self.remade.is_empty()
            && let Ok(at) = self.remade.binary_search_by_key(&cell, |remade| remade.0)
        {
            return &self.remade[at].1;
        }
        self.base(cell)
    }

    /// Heap bytes the lists hold, the shared base included.
    pub fn bytes(&self) -> usize {
        (self.cells.capacity() + self.offsets.capacity() + self.owners.capacity())
            * size_of::<u32>()
            + (self.placed.capacity() + self.moved.capacity()) * size_of::<(u32, u32)>()
            + self
                .remade
                .iter()
                .map(|(_, list)| list.capacity() * size_of::<u32>())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Owners with their cells and stamps; `None` is an erased owner.
    struct Model {
        owners: Vec<(Option<Vec<u32>>, u32)>,
    }

    impl Rows for Model {
        fn owners(&self, each: &mut dyn FnMut(u32)) {
            for (owner, (cells, _)) in self.owners.iter().enumerate() {
                if cells.is_some() {
                    each(owner as u32);
                }
            }
        }

        fn stamp(&self, owner: u32) -> u32 {
            self.owners[owner as usize].1
        }

        fn entries(&self, owner: u32, each: &mut dyn FnMut(u32, u32)) {
            for (index, cell) in self.owners[owner as usize].0.iter().flatten().enumerate() {
                each(*cell, index as u32);
            }
        }
    }

    proptest! {
        /// Whatever was written between lookups, a lookup answers exactly the
        /// entries holding the cell.
        #[test]
        fn a_lookup_answers_the_entries_holding_the_cell(
            edits in prop::collection::vec(
                (0u32..40, prop::option::of(prop::collection::vec(0u32..6, 0..4)), any::<bool>()),
                0..400,
            ),
        ) {
            let mut model = Model { owners: (0..40).map(|_| (Some(Vec::new()), 0)).collect() };
            let mut grouping = Grouping::default();
            let mut log = Vec::new();
            let mut stamp = 1;
            for (owner, cells, look) in edits {
                model.owners[owner as usize] = (cells, stamp);
                log.push(owner);
                if !look {
                    continue;
                }
                grouping.refresh(&model, &log, stamp);
                stamp += 1;
                for cell in 0..6 {
                    let mut found = Vec::new();
                    grouping.read(&model, cell, &mut found);
                    let mut expected = Vec::new();
                    model.owners(&mut |owner| model.entries(owner, &mut |held, index| {
                        if held == cell {
                            expected.push((owner, index));
                        }
                    }));
                    prop_assert_eq!(grouping.any(&model, cell), !expected.is_empty());
                    found.sort_unstable();
                    prop_assert_eq!(found, expected);
                }
            }
        }

        /// However owners joined, left and moved between lookups, a list
        /// reads back its owners in position order.
        #[test]
        fn a_list_reads_back_its_owners_in_position_order(
            edits in prop::collection::vec(
                (0u32..200, prop::option::of((0u32..6, 0u32..1000)), any::<bool>()),
                0..600,
            ),
        ) {
            // A placed owner's one "cell" is its list and its "index" its
            // position, which the model keeps as a one-entry list.
            let mut model = Model { owners: (0..200).map(|_| (None, 0)).collect() };
            let mut places: Vec<Option<(u32, u32)>> = vec![None; 200];
            let mut lists = Lists::default();
            let mut log = Vec::new();
            struct Placed<'a>(&'a [Option<(u32, u32)>]);
            impl Rows for Placed<'_> {
                fn owners(&self, each: &mut dyn FnMut(u32)) {
                    (0..self.0.len() as u32).for_each(each);
                }
                fn stamp(&self, _: u32) -> u32 {
                    0
                }
                fn entries(&self, owner: u32, each: &mut dyn FnMut(u32, u32)) {
                    if let Some((cell, position)) = self.0[owner as usize] {
                        each(cell, position);
                    }
                }
            }
            for (owner, place, look) in edits {
                places[owner as usize] = place;
                model.owners[owner as usize].1 += 1;
                log.push(owner);
                if !look {
                    continue;
                }
                lists.refresh(&Placed(&places), &log);
                for cell in 0..6 {
                    let mut expected: Vec<(u32, u32)> = places
                        .iter()
                        .enumerate()
                        .filter_map(|(owner, place)| match place {
                            Some((held, position)) if *held == cell => Some((*position, owner as u32)),
                            _ => None,
                        })
                        .collect();
                    expected.sort_unstable();
                    let expected: Vec<u32> = expected.into_iter().map(|(_, owner)| owner).collect();
                    prop_assert_eq!(lists.read(cell), &expected[..]);
                }
            }
        }
    }
}
