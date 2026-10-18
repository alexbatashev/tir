//! Chunked storage for relations keyed by a row id: positional columns and
//! clustered tables, cut into fixed-size chunks.
//!
//! An id is `chunk << CHUNK_BITS | row`. A [`Chunk`] holds up to
//! [`CHUNK_ROWS`] rows of one id space: `P` positional columns indexed by the
//! row, `S` clustered tables whose rows are sorted by their owner row and found
//! through an offsets column, a liveness bit and a stamp per row, and whatever
//! per-row payload the client keeps that is not a `u32`. A chunk has one owner,
//! the partition whose rows it holds, so everything derived from a partition is
//! derived from its chunks alone.
//!
//! A [`Space`] is the chunks of one id space behind shared pointers. Cloning a
//! space copies pointers; writing to a chunk another clone still reads copies
//! that chunk first, so a clone is a snapshot and a writer never waits for a
//! reader.

use std::sync::Arc;

pub const CHUNK_BITS: u32 = 12;
/// Rows one chunk holds.
pub const CHUNK_ROWS: usize = 1 << CHUNK_BITS;

/// The chunk an id falls in.
#[inline(always)]
pub const fn chunk_of(id: u32) -> u32 {
    id >> CHUNK_BITS
}

/// The row an id names inside its chunk.
#[inline(always)]
pub const fn row_of(id: u32) -> usize {
    id as usize & (CHUNK_ROWS - 1)
}

/// The id of `row` in `chunk`.
#[inline(always)]
pub const fn id_at(chunk: u32, row: usize) -> u32 {
    chunk << CHUNK_BITS | row as u32
}

/// A clustered table of one chunk: the cells of every owner row back to back,
/// in row order, with the offset each owner's cells start at. A cell's index
/// inside its owner is its distance from that offset.
#[derive(Clone, Debug)]
pub struct Segment {
    /// One entry per row and one closing the last.
    start: Vec<u32>,
    cells: Vec<u32>,
}

impl Default for Segment {
    fn default() -> Self {
        Segment {
            start: vec![0],
            cells: Vec::new(),
        }
    }
}

impl Segment {
    /// The cells `row` owns.
    #[inline(always)]
    pub fn of(&self, row: usize) -> &[u32] {
        &self.cells[self.start[row] as usize..self.start[row + 1] as usize]
    }

    /// Every cell of the chunk, owners in row order.
    pub fn cells(&self) -> &[u32] {
        &self.cells
    }

    /// Where each row's cells start in [`Self::cells`], with one more entry
    /// closing the last row.
    pub fn starts(&self) -> &[u32] {
        &self.start
    }

    pub fn set(&mut self, row: usize, index: usize, cell: u32) {
        debug_assert!(index < self.of(row).len());
        self.cells[self.start[row] as usize + index] = cell;
    }

    /// Replace `remove` cells of `row` from `at` with `insert`. The cells of
    /// later rows move by the difference.
    pub fn splice(&mut self, row: usize, at: usize, remove: usize, insert: &[u32]) {
        let from = self.start[row] as usize + at;
        debug_assert!(from + remove <= self.start[row + 1] as usize);
        self.cells
            .splice(from..from + remove, insert.iter().copied());
        if insert.len() != remove {
            let delta = insert.len() as u32;
            for start in &mut self.start[row + 1..] {
                *start = *start + delta - remove as u32;
            }
        }
    }

    /// Give `row` exactly `cells`.
    pub fn replace(&mut self, row: usize, cells: &[u32]) {
        self.splice(row, 0, self.of(row).len(), cells);
    }

    fn push_row(&mut self) {
        self.start.push(self.cells.len() as u32);
    }

    fn bytes(&self) -> usize {
        (self.start.capacity() + self.cells.capacity()) * size_of::<u32>()
    }
}

/// What a chunk keeps per row beside its `u32` cells.
pub trait Payload: Clone + Default {
    /// Make room for one more row.
    fn push_row(&mut self);
    /// Take `row` as `from` has it.
    fn copy_row(&mut self, from: &Self, row: usize);
    /// Heap bytes held.
    fn bytes(&self) -> usize;
}

impl Payload for () {
    fn push_row(&mut self) {}
    fn copy_row(&mut self, _: &Self, _: usize) {}
    fn bytes(&self) -> usize {
        0
    }
}

/// Up to [`CHUNK_ROWS`] rows of one id space, column-major.
#[derive(Clone, Debug)]
pub struct Chunk<const P: usize, const S: usize, X = ()> {
    /// The partition whose rows these are.
    pub owner: u32,
    /// The other partitions with a row naming an id of this chunk.
    pub foreign: Vec<u32>,
    /// The stamp of the first write since the client last looked; the client
    /// reads and resets it to know which chunks an epoch wrote.
    pub touched: u32,
    live: [u64; CHUNK_ROWS / 64],
    dead: u32,
    stamp: Vec<u32>,
    cols: [Vec<u32>; P],
    segs: [Segment; S],
    pub extra: X,
}

impl<const P: usize, const S: usize, X: Payload> Chunk<P, S, X> {
    pub fn new(owner: u32) -> Self {
        Chunk {
            owner,
            foreign: Vec::new(),
            touched: 0,
            live: [0; CHUNK_ROWS / 64],
            dead: 0,
            stamp: Vec::new(),
            cols: std::array::from_fn(|_| Vec::new()),
            segs: std::array::from_fn(|_| Segment::default()),
            extra: X::default(),
        }
    }

    /// Rows minted, live or not.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.stamp.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stamp.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.len() == CHUNK_ROWS
    }

    /// Rows minted and not erased.
    pub fn live_rows(&self) -> usize {
        self.len() - self.dead as usize
    }

    /// Append a live row holding `cells` and owning no clustered cell.
    pub fn push(&mut self, cells: [u32; P], stamp: u32) -> usize {
        let row = self.len();
        assert!(row < CHUNK_ROWS, "a full chunk takes no row");
        for (column, cell) in self.cols.iter_mut().zip(cells) {
            column.push(cell);
        }
        for segment in &mut self.segs {
            segment.push_row();
        }
        self.extra.push_row();
        self.stamp.push(stamp);
        self.live[row / 64] |= 1 << (row % 64);
        row
    }

    #[inline(always)]
    pub fn is_live(&self, row: usize) -> bool {
        self.live[row / 64] >> (row % 64) & 1 != 0 && row < self.len()
    }

    /// Erase `row`. Its cells stay until [`Self::compact`]; a scan masks them
    /// by [`Self::is_live`].
    pub fn kill(&mut self, row: usize) {
        if self.is_live(row) {
            self.live[row / 64] &= !(1 << (row % 64));
            self.dead += 1;
        }
    }

    /// One positional column, a cell per row minted.
    #[inline(always)]
    pub fn col(&self, column: usize) -> &[u32] {
        &self.cols[column]
    }

    #[inline(always)]
    pub fn cell(&self, column: usize, row: usize) -> u32 {
        self.cols[column][row]
    }

    #[inline(always)]
    pub fn set(&mut self, column: usize, row: usize, cell: u32) {
        self.cols[column][row] = cell;
    }

    /// The stamp each row was last written at.
    pub fn stamps(&self) -> &[u32] {
        &self.stamp
    }

    #[inline(always)]
    pub fn stamp(&self, row: usize) -> u32 {
        self.stamp[row]
    }

    /// Stamp `row`, reporting whether the stamp moved.
    #[inline(always)]
    pub fn set_stamp(&mut self, row: usize, stamp: u32) -> bool {
        std::mem::replace(&mut self.stamp[row], stamp) != stamp
    }

    #[inline(always)]
    pub fn seg(&self, segment: usize) -> &Segment {
        &self.segs[segment]
    }

    pub fn seg_mut(&mut self, segment: usize) -> &mut Segment {
        &mut self.segs[segment]
    }

    /// Whether enough rows died that their clustered cells are worth dropping.
    pub fn wants_compaction(&self) -> bool {
        self.dead as usize * 4 > self.len()
            && self.segs.iter().any(|segment| !segment.cells.is_empty())
    }

    /// Drop the clustered cells of erased rows. Ids do not move: an erased
    /// row keeps its number and owns nothing.
    pub fn compact(&mut self) {
        for segment in &mut self.segs {
            let mut kept = Vec::with_capacity(segment.cells.len());
            let mut start = Vec::with_capacity(segment.start.len());
            for row in 0..self.stamp.len() {
                start.push(kept.len() as u32);
                if self.live[row / 64] >> (row % 64) & 1 != 0 {
                    kept.extend_from_slice(segment.of(row));
                }
            }
            start.push(kept.len() as u32);
            segment.cells = kept;
            segment.start = start;
        }
    }

    /// Take `row` as `from` holds it: its cells, its clustered cells, its
    /// payload, its stamp and whether it lives. `from` is another copy of this
    /// chunk, so the row exists in both or is appended here in order.
    pub fn copy_row(&mut self, from: &Self, row: usize) {
        if row == self.len() {
            self.push([0; P], 0);
        }
        for (column, theirs) in self.cols.iter_mut().zip(&from.cols) {
            column[row] = theirs[row];
        }
        for (segment, theirs) in self.segs.iter_mut().zip(&from.segs) {
            segment.replace(row, theirs.of(row));
        }
        self.extra.copy_row(&from.extra, row);
        self.stamp[row] = from.stamp[row];
        match from.is_live(row) {
            true if !self.is_live(row) => {
                self.live[row / 64] |= 1 << (row % 64);
                self.dead -= 1;
            }
            true => {}
            false => self.kill(row),
        }
    }

    /// Whether the chunk already knows partition `other` names its ids.
    pub fn names_from(&self, other: u32) -> bool {
        other == self.owner || self.foreign.contains(&other)
    }

    /// Heap bytes the chunk holds.
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.stamp.capacity() * size_of::<u32>()
            + self
                .cols
                .iter()
                .map(|column| column.capacity() * size_of::<u32>())
                .sum::<usize>()
            + self.segs.iter().map(Segment::bytes).sum::<usize>()
            + self.extra.bytes()
    }
}

const PAGE_BITS: u32 = 5;
const PAGE_SLOTS: usize = 1 << PAGE_BITS;

struct Page<T>([Option<Arc<T>>; PAGE_SLOTS]);

impl<T> Clone for Page<T> {
    fn clone(&self) -> Self {
        Page(self.0.clone())
    }
}

/// A vector of shared values, itself shared by the page: a clone copies one
/// pointer per page, and a write copies the page and the value it reaches
/// unless this vector is their only holder.
pub struct Paged<T> {
    pages: Vec<Arc<Page<T>>>,
    len: usize,
}

impl<T> Clone for Paged<T> {
    fn clone(&self) -> Self {
        Paged {
            pages: self.pages.clone(),
            len: self.len,
        }
    }
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Paged {
            pages: Vec::new(),
            len: 0,
        }
    }
}

impl<T: Clone> Paged<T> {
    /// One past the highest slot ever filled.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The shared value at `index`, if the slot is filled.
    #[inline(always)]
    pub fn slot(&self, index: usize) -> Option<&Arc<T>> {
        self.pages.get(index >> PAGE_BITS)?.0[index & (PAGE_SLOTS - 1)].as_ref()
    }

    #[inline(always)]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.slot(index).map(|value| &**value)
    }

    /// The value at `index` for writing, copied first if anything else holds
    /// it.
    #[inline(always)]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        let page = Arc::make_mut(self.pages.get_mut(index >> PAGE_BITS)?);
        page.0[index & (PAGE_SLOTS - 1)].as_mut().map(Arc::make_mut)
    }

    /// Fill slot `index`, leaving the slots skipped on the way empty.
    pub fn put(&mut self, index: usize, value: Arc<T>) {
        while self.pages.len() <= index >> PAGE_BITS {
            self.pages
                .push(Arc::new(Page(std::array::from_fn(|_| None))));
        }
        Arc::make_mut(&mut self.pages[index >> PAGE_BITS]).0[index & (PAGE_SLOTS - 1)] =
            Some(value);
        self.len = self.len.max(index + 1);
    }

    /// The filled slots, ascending.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        (0..self.len).filter_map(|index| Some((index, self.get(index)?)))
    }
}

/// The chunks of one id space.
pub struct Space<const P: usize, const S: usize, X = ()>(Paged<Chunk<P, S, X>>);

impl<const P: usize, const S: usize, X> Clone for Space<P, S, X> {
    fn clone(&self) -> Self {
        Space(self.0.clone())
    }
}

impl<const P: usize, const S: usize, X> Default for Space<P, S, X> {
    fn default() -> Self {
        Space(Paged::default())
    }
}

impl<const P: usize, const S: usize, X: Payload> Space<P, S, X> {
    /// One past the highest chunk index in use.
    pub fn chunks(&self) -> u32 {
        self.0.len() as u32
    }

    /// The chunk at `index`. Panics for an index no chunk was opened at: an
    /// id is read after it was minted.
    #[inline(always)]
    pub fn chunk(&self, index: u32) -> &Chunk<P, S, X> {
        self.0
            .get(index as usize)
            .expect("the chunk of a minted id")
    }

    #[inline(always)]
    pub fn find(&self, index: u32) -> Option<&Chunk<P, S, X>> {
        self.0.get(index as usize)
    }

    #[inline(always)]
    pub fn chunk_mut(&mut self, index: u32) -> &mut Chunk<P, S, X> {
        self.0
            .get_mut(index as usize)
            .expect("the chunk of a minted id")
    }

    pub fn shared(&self, index: u32) -> Option<&Arc<Chunk<P, S, X>>> {
        self.0.slot(index as usize)
    }

    /// Put `chunk` at `index`, replacing what was there.
    pub fn put(&mut self, index: u32, chunk: Arc<Chunk<P, S, X>>) {
        self.0.put(index as usize, chunk);
    }

    /// Whether `id` names a row minted and not erased.
    #[inline(always)]
    pub fn is_live(&self, id: u32) -> bool {
        self.find(chunk_of(id))
            .is_some_and(|chunk| chunk.is_live(row_of(id)))
    }

    /// The chunks in use, ascending by index.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Chunk<P, S, X>)> {
        self.0.iter().map(|(index, chunk)| (index as u32, chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_one_owner_moves_the_owners_after_it() {
        let mut chunk: Chunk<1, 1> = Chunk::new(0);
        for row in 0..3 {
            chunk.push([row], 1);
            chunk
                .seg_mut(0)
                .replace(row as usize, &[row * 10, row * 10 + 1]);
        }
        chunk.seg_mut(0).splice(0, 1, 0, &[7, 8]);
        chunk.seg_mut(0).splice(1, 0, 1, &[]);
        assert_eq!(chunk.seg(0).of(0), &[0, 7, 8, 1]);
        assert_eq!(chunk.seg(0).of(1), &[11]);
        assert_eq!(chunk.seg(0).of(2), &[20, 21]);
    }

    #[test]
    fn a_clone_keeps_reading_what_it_was_cloned_from() {
        let mut space: Space<1, 0> = Space::default();
        let mut chunk = Chunk::new(0);
        chunk.push([5], 1);
        space.put(3, Arc::new(chunk));
        let snapshot = space.clone();
        space.chunk_mut(3).set(0, 0, 6);
        assert_eq!(snapshot.chunk(3).cell(0, 0), 5);
        assert_eq!(space.chunk(3).cell(0, 0), 6);
        assert!(space.find(2).is_none());
    }

    #[test]
    fn compaction_drops_the_cells_of_erased_rows_and_keeps_ids() {
        let mut chunk: Chunk<0, 1> = Chunk::new(0);
        for row in 0..4 {
            chunk.push([], 1);
            chunk.seg_mut(0).replace(row, &[row as u32; 3]);
        }
        chunk.kill(1);
        chunk.kill(2);
        assert!(chunk.wants_compaction());
        chunk.compact();
        assert_eq!(chunk.seg(0).cells().len(), 6);
        assert_eq!(chunk.seg(0).of(3), &[3, 3, 3]);
        assert!(chunk.seg(0).of(1).is_empty());
    }
}
