//! The saturated selection graph as columns.
//!
//! Once saturation ends nothing merges, so a function's graph is read-only
//! for the rest of selection. Matching reads it far more often than the
//! engine's tables are laid out for: every pattern rooted at an operator asks
//! each row of that operator for its operands. This is the same rows, grouped
//! by class and stored as plain columns, with every reference canonical.

use smallvec::SmallVec;
use tir::TypeId;
use tir::sem::egraph::SemEGraph;
use tir_egraph::{ClassId as Id, Ref};

/// The operator of a row no semantic pattern names.
pub(crate) const NO_KIND: u16 = u16::MAX;

/// What a pattern asks of one operand of a row.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Operand {
    Any,
    /// A row of the shape is at the operand's value.
    Row(u32),
    /// A row of the shape is at offset zero of the operand's class.
    RowAtZero(u32),
}

/// What a pattern asks of a row and the rows under it, operands' constraints
/// aside: the question many patterns share.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Shape {
    pub(crate) kind: u16,
    pub(crate) ty: Option<TypeId>,
    pub(crate) commutes: bool,
    pub(crate) operands: SmallVec<[Operand; 4]>,
}

/// The shapes of a target's patterns, each after the shapes its operands
/// name.
#[derive(Default)]
pub(crate) struct Shapes {
    shapes: Vec<Shape>,
    index: std::collections::HashMap<Shape, u32>,
}

impl Shapes {
    pub(crate) fn intern(&mut self, shape: Shape) -> u32 {
        *self.index.entry(shape.clone()).or_insert_with(|| {
            self.shapes.push(shape);
            self.shapes.len() as u32 - 1
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.shapes.len()
    }
}

pub(crate) struct Frozen {
    /// `start[class]..start[class + 1]` are the rows of a class, in the
    /// engine's order. Only a representative class has any.
    start: Vec<u32>,
    pub(crate) kind: Vec<u16>,
    pub(crate) ty: Vec<Option<TypeId>>,
    /// The offset each row's value sits at in its class.
    pub(crate) offset: Vec<u64>,
    operands: Vec<u32>,
    children: Vec<Ref>,
    /// One bit per row and shape, `words` words a row.
    shaped: Vec<u64>,
    words: usize,
}

impl Frozen {
    pub(crate) fn new(egraph: &SemEGraph, shapes: &Shapes) -> Self {
        let classes = egraph.class_count();
        let rows = egraph.total_size();
        let mut frozen = Self {
            start: Vec::with_capacity(classes + 1),
            kind: Vec::with_capacity(rows),
            ty: Vec::with_capacity(rows),
            offset: Vec::with_capacity(rows),
            operands: Vec::with_capacity(rows + 1),
            children: Vec::with_capacity(rows * 2),
            shaped: Vec::new(),
            words: shapes.len().div_ceil(64),
        };
        let mut labels = Vec::new();
        for class in (0..classes as u32).map(Id) {
            frozen.start.push(frozen.kind.len() as u32);
            if egraph.root(class) != class {
                continue;
            }
            // A class a merge made congruent rows meet in lists each of them;
            // they are one row here.
            labels.clear();
            let first = frozen.kind.len();
            for row in egraph.rows(class) {
                let node = egraph.node(row);
                let label = egraph.label(row);
                let offset = egraph.value(row).offset;
                let children = egraph.children(row);
                let repeated = (first..frozen.kind.len()).any(|earlier| {
                    labels[earlier - first] == label
                        && frozen.offset[earlier] == offset
                        && frozen.children[frozen.operands[earlier] as usize..][..children
                            .len()
                            .min(frozen.children.len() - frozen.operands[earlier] as usize)]
                            == children[..]
                        && frozen
                            .operands
                            .get(earlier + 1)
                            .map_or(frozen.children.len(), |&end| end as usize)
                            - frozen.operands[earlier] as usize
                            == children.len()
                });
                if repeated {
                    continue;
                }
                labels.push(label);
                frozen
                    .kind
                    .push(node.sym().map_or(NO_KIND, |kind| kind as u16));
                frozen.ty.push(node.ty);
                frozen.offset.push(offset);
                frozen.operands.push(frozen.children.len() as u32);
                frozen.children.extend(children);
            }
        }
        frozen.start.push(frozen.kind.len() as u32);
        frozen.operands.push(frozen.children.len() as u32);
        frozen.shape(shapes);
        frozen
    }

    /// Answer every shape for every row. A shape reads only the answers of
    /// the shapes before it, so one pass per shape over the rows of its
    /// operator settles it, cycles in the graph included.
    fn shape(&mut self, shapes: &Shapes) {
        self.shaped = vec![0; self.kind.len() * self.words];
        let mut by_kind: Vec<(u16, u32)> = (0..self.kind.len() as u32)
            .map(|row| (self.kind[row as usize], row))
            .collect();
        by_kind.sort_unstable();
        for (index, shape) in shapes.shapes.iter().enumerate() {
            let first = by_kind.partition_point(|&(kind, _)| kind < shape.kind);
            for &(kind, row) in &by_kind[first..] {
                if kind != shape.kind {
                    break;
                }
                let row = row as usize;
                if shape.ty.is_some() && self.ty[row] != shape.ty {
                    continue;
                }
                let children = self.children(row);
                if children.len() != shape.operands.len() {
                    continue;
                }
                let holds = |swapped: bool| {
                    shape.operands.iter().enumerate().all(|(at, operand)| {
                        let child = children[if swapped { 1 - at } else { at }];
                        let (shape, offset) = match *operand {
                            Operand::Any => return true,
                            Operand::Row(shape) => (shape, child.offset),
                            Operand::RowAtZero(shape) => (shape, 0),
                        };
                        self.rows(child.class)
                            .any(|row| self.offset[row] == offset && self.has_shape(row, shape))
                    })
                };
                if holds(false) || (shape.commutes && holds(true)) {
                    self.shaped[row * self.words + index / 64] |= 1 << (index % 64);
                }
            }
        }
    }

    pub(crate) fn has_shape(&self, row: usize, shape: u32) -> bool {
        let shape = shape as usize;
        self.shaped[row * self.words + shape / 64] & (1 << (shape % 64)) != 0
    }

    /// The shapes `row` has, ascending.
    pub(crate) fn shapes_of(&self, row: usize) -> impl Iterator<Item = u32> + '_ {
        self.shaped[row * self.words..(row + 1) * self.words]
            .iter()
            .enumerate()
            .flat_map(|(word, &bits)| {
                std::iter::successors((bits != 0).then_some(bits), |bits| {
                    let rest = bits & (bits - 1);
                    (rest != 0).then_some(rest)
                })
                .map(move |bits| word as u32 * 64 + bits.trailing_zeros())
            })
    }

    pub(crate) fn rows(&self, class: Id) -> std::ops::Range<usize> {
        self.start[class.index()] as usize..self.start[class.index() + 1] as usize
    }

    pub(crate) fn children(&self, row: usize) -> &[Ref] {
        &self.children[self.operands[row] as usize..self.operands[row + 1] as usize]
    }
}
