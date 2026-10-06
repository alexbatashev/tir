//! A table sorted by a column order and read as a trie: one level per column,
//! each node the distinct cells under the prefix above it, the rows at the
//! leaves. What a join intersects and descends through.

use smallvec::SmallVec;

use super::set::Set;
use crate::store::Table;

/// The sorted projection of a table onto a column order.
#[derive(Debug, Default)]
pub(crate) struct Trie {
    /// The column read at each level.
    columns: SmallVec<[u8; 6]>,
    levels: Vec<Level>,
    /// The rows sorted by the columns, then ascending. Leaf `n`'s rows are
    /// `rows[leaves[n]..leaves[n + 1]]`.
    rows: Vec<u32>,
    leaves: Vec<u32>,
    /// The words of every node laid out as a bitset.
    bits: Vec<u64>,
}

/// One level: node `n` holds `cells[starts[n]..starts[n + 1]]`, and the node
/// below its `i`-th cell is node `starts[n] + i` of the next level (or that
/// leaf, under the last level).
#[derive(Debug, Default)]
struct Level {
    starts: Vec<u32>,
    cells: Vec<u32>,
    /// Per node, its range of the bitset words; empty for a node kept only as
    /// a list.
    words: Vec<(u32, u32)>,
}

/// A node of a trie: its level and its index there. Level `columns.len()` is
/// the leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    level: u32,
    index: u32,
}

impl Node {
    /// How many columns lie above the node: the index of the column its cells
    /// are of.
    pub(crate) fn depth(self) -> usize {
        self.level as usize
    }
}

impl Trie {
    /// Index `table` by `columns`, reusing this trie's storage.
    pub(crate) fn build(&mut self, table: &Table, columns: &[u8]) {
        self.columns = SmallVec::from_slice(columns);
        let columns: SmallVec<[&[u32]; 6]> = columns
            .iter()
            .map(|&column| table.column(column as usize))
            .collect();
        self.rows.clear();
        self.rows.extend(0..table.len() as u32);
        self.rows.sort_unstable_by(|&a, &b| {
            columns
                .iter()
                .map(|column| column[a as usize].cmp(&column[b as usize]))
                .find(|order| order.is_ne())
                .unwrap_or(a.cmp(&b))
        });
        self.levels.resize_with(columns.len(), Level::default);
        for level in &mut self.levels {
            level.starts.clear();
            level.cells.clear();
            level.words.clear();
        }
        self.levels[0].starts.push(0);
        self.leaves.clear();
        for (at, &row) in self.rows.iter().enumerate() {
            // The first column this row differs from the one before in opens
            // a cell there and a node at every level below.
            let differs = match at {
                0 => 0,
                _ => {
                    let before = self.rows[at - 1] as usize;
                    let row = row as usize;
                    columns
                        .iter()
                        .position(|column| column[before] != column[row])
                        .unwrap_or(columns.len())
                }
            };
            for (depth, level) in self.levels.iter_mut().enumerate().skip(differs) {
                if depth > differs {
                    level.starts.push(level.cells.len() as u32);
                }
                level.cells.push(columns[depth][row as usize]);
            }
            if differs < columns.len() {
                self.leaves.push(at as u32);
            }
        }
        for level in &mut self.levels {
            level.starts.push(level.cells.len() as u32);
        }
        self.leaves.push(self.rows.len() as u32);
        self.bits.clear();
        for level in &mut self.levels {
            for node in level.starts.windows(2) {
                let cells = &level.cells[node[0] as usize..node[1] as usize];
                let from = self.bits.len() as u32;
                if let (Some(&min), Some(&max)) = (cells.first(), cells.last()) {
                    let base = min / 64 * 64;
                    let words = (max - base) as usize / 64 + 1;
                    // Bits when the words take no more room than the list.
                    if words * 2 <= cells.len() {
                        self.bits.resize(self.bits.len() + words, 0);
                        let words = &mut self.bits[from as usize..];
                        for &cell in cells {
                            words[(cell - base) as usize / 64] |= 1 << ((cell - base) % 64);
                        }
                    }
                }
                level.words.push((from, self.bits.len() as u32));
            }
        }
    }

    pub(crate) fn columns(&self) -> &[u8] {
        &self.columns
    }

    pub(crate) fn root(&self) -> Node {
        Node { level: 0, index: 0 }
    }

    /// The cells of a node above the leaves.
    pub(crate) fn set(&self, node: Node) -> Set<'_> {
        let level = &self.levels[node.level as usize];
        let at = node.index as usize;
        let cells = &level.cells[level.starts[at] as usize..level.starts[at + 1] as usize];
        match level.words[at] {
            (from, to) if from < to => Set::Bits {
                base: cells[0] / 64 * 64,
                words: &self.bits[from as usize..to as usize],
                len: cells.len() as u32,
            },
            _ => Set::Uint(cells),
        }
    }

    /// The node under `cell` of a node above the leaves.
    pub(crate) fn child(&self, node: Node, cell: u32) -> Option<Node> {
        let level = &self.levels[node.level as usize];
        let at = node.index as usize;
        let from = level.starts[at] as usize;
        let cells = &level.cells[from..level.starts[at + 1] as usize];
        let found = cells.binary_search(&cell).ok()?;
        Some(Node {
            level: node.level + 1,
            index: (from + found) as u32,
        })
    }

    /// The rows of a leaf, ascending.
    pub(crate) fn rows(&self, leaf: Node) -> &[u32] {
        debug_assert_eq!(leaf.level as usize, self.levels.len(), "a leaf");
        let at = leaf.index as usize;
        &self.rows[self.leaves[at] as usize..self.leaves[at + 1] as usize]
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Walk every path of the trie, checking each node's set against its
    /// cells, and collect `(path, rows)` per leaf.
    fn leaves(trie: &Trie, node: Node, path: &mut Vec<u32>, out: &mut Vec<(Vec<u32>, Vec<u32>)>) {
        if node.depth() == trie.columns().len() {
            out.push((path.clone(), trie.rows(node).to_vec()));
            return;
        }
        let mut cells = Vec::new();
        trie.set(node).list(&mut cells);
        assert!(
            cells.windows(2).all(|pair| pair[0] < pair[1]),
            "ascending, distinct"
        );
        assert_eq!(trie.set(node).len(), cells.len());
        for cell in cells {
            path.push(cell);
            leaves(
                trie,
                trie.child(node, cell).expect("a listed cell has a child"),
                path,
                out,
            );
            path.pop();
        }
    }

    proptest! {
        /// Every leaf is one distinct projection of the rows, in sorted order,
        /// holding exactly the rows with it, ascending; a cell no row has has
        /// no child.
        #[test]
        fn a_trie_is_the_sorted_projection(
            rows in prop::collection::vec((0u32..3, 0u32..200, 0u32..6, 0u32..40), 0..300),
            keyed in any::<bool>(),
            columns in prop::sample::select(vec![vec![0u8, 3, 1, 2], vec![0, 3, 2, 1], vec![2, 0], vec![1]]),
        ) {
            let mut table = if keyed { Table::new(3) } else { Table::bag(3) };
            for &(a, b, c, value) in &rows {
                if !keyed || table.get(&[a, b, c]).is_none() {
                    table.insert(&[a, b, c], value, 0, 1);
                }
            }
            let mut trie = Trie::default();
            trie.build(&table, &columns);
            let mut want: Vec<(Vec<u32>, Vec<u32>)> = Vec::new();
            let mut sorted: Vec<(Vec<u32>, u32)> = (0..table.len() as u32)
                .map(|row| (columns.iter().map(|&c| table.column(c as usize)[row as usize]).collect(), row))
                .collect();
            sorted.sort();
            for (path, row) in sorted {
                match want.last_mut() {
                    Some((last, rows)) if *last == path => rows.push(row),
                    _ => want.push((path, vec![row])),
                }
            }
            let mut got = Vec::new();
            leaves(&trie, trie.root(), &mut Vec::new(), &mut got);
            prop_assert_eq!(got, want);
            prop_assert_eq!(trie.child(trie.root(), 1000), None);
        }
    }
}
