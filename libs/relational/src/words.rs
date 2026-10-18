//! Client tables: relations over words a client builds whole, which a query
//! ranges over like any other table.

use std::sync::OnceLock;

use crate::query::trie::Trie;

/// A relation of `width` columns of 32-bit words. A client fills it before handing
/// it to a database and nothing writes it after, so an atom over it is never
/// new in a round. Its cells name nothing a database renames: no id space
/// covers them. A query meets its rows in the order they were added, and a
/// row added twice twice.
#[derive(Debug)]
pub struct Words {
    columns: Vec<Vec<u32>>,
    // Every row, ascending.
    rows: Vec<u32>,
    // The tries queries asked for, each built once for every database that
    // reads the table.
    tries: OnceLock<Box<Index>>,
}

#[derive(Debug)]
struct Index {
    columns: Vec<u8>,
    trie: Trie,
    next: OnceLock<Box<Index>>,
}

impl Words {
    pub fn new(width: usize) -> Self {
        assert!(width > 0, "a relation has a column");
        Self {
            columns: vec![Vec::new(); width],
            rows: Vec::new(),
            tries: OnceLock::new(),
        }
    }

    pub fn width(&self) -> usize {
        self.columns.len()
    }

    pub fn len(&self) -> usize {
        self.columns[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns[0].is_empty()
    }

    /// Add `row`, one cell per column, after every row added before.
    pub fn push(&mut self, row: &[u32]) {
        assert_eq!(row.len(), self.width());
        self.rows.push(self.len() as u32);
        for (column, &cell) in self.columns.iter_mut().zip(row) {
            column.push(cell);
        }
        self.tries = OnceLock::new();
    }

    /// The cells of column `index`, one per row.
    pub fn column(&self, index: usize) -> &[u32] {
        &self.columns[index]
    }

    pub(crate) fn rows(&self) -> &[u32] {
        &self.rows
    }

    // `columns` must not be empty.
    pub(crate) fn trie(&self, columns: &[u8]) -> &Trie {
        let mut at = &self.tries;
        loop {
            let index = at.get_or_init(|| {
                let cells: Vec<&[u32]> = columns.iter().map(|&c| self.column(c as usize)).collect();
                Box::new(Index {
                    columns: columns.to_vec(),
                    trie: Trie::fixed(&cells, self.len()),
                    next: OnceLock::new(),
                })
            });
            if index.columns == columns {
                return &index.trie;
            }
            at = &index.next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_reads_its_rows_in_the_order_added() {
        let mut table = Words::new(3);
        for row in 0..100u32 {
            table.push(&[row % 5, row % 3, row]);
        }
        let trie = table.trie(&[1, 0]);
        let leaf = [2, 4]
            .iter()
            .try_fold(trie.root(), |node, &cell| trie.child(node, cell));
        assert_eq!(trie.rows(leaf.unwrap()), &[14, 29, 44, 59, 74, 89]);
        assert_eq!(trie.child(trie.root(), 3), None);
        assert!(std::ptr::eq(trie, table.trie(&[1, 0])));
        assert_eq!(Words::new(1).trie(&[0]).set(trie.root()).len(), 0);
    }
}
