//! A rule as data: a query, and a head that only writes.
//!
//! Nothing in a head reads the graph and nothing outside an atom reads it
//! either, so a match exists in round `t` and not `t-1` exactly when one of its
//! rows or facts is from round `t-1`'s delta. That is what a hand-asserted
//! "this applier reads no deeper than its pattern" used to promise and nothing
//! checked.

use smallvec::SmallVec;

use tir_relational::Database;
use tir_relational::query::{Expr, Scalar, Var};
use tir_relational::rule::{self as rel, Client, Head, Key, Keyed};

use crate::engine::{CLASSES, Lists, Shaped};
use crate::query::{Externs, Field, Match, NoExterns, Plan};
use crate::{ClassId, Engine, Label, LabelId, Ref};

/// A node a head builds: a template with scalars written into named fields.
#[derive(Clone, Debug)]
pub struct LabelFill<L> {
    pub template: L,
    pub fills: SmallVec<[(Field, Scalar); 2]>,
}

impl<L> LabelFill<L> {
    pub fn plain(template: L) -> Self {
        Self {
            template,
            fills: SmallVec::new(),
        }
    }
}

/// One write of a rule's right-hand side. Variables hold references.
#[derive(Clone, Debug)]
pub enum HeadOp<L> {
    /// Hash-cons `label(args)` and bind its value to `into`.
    Insert {
        label: LabelFill<L>,
        args: SmallVec<[Var; 4]>,
        into: Var,
    },
    Union(Var, Var),
    /// Bind `into` to `base` plus `offset`.
    Offset {
        base: Var,
        offset: Expr,
        into: Var,
    },
    /// Union `class` with the variable `offset + scalars[index]` — the one head
    /// that chooses among bound variables, for a rule that picks an operand by
    /// a guard's answer (which arm of a decided gate).
    UnionIndexed {
        class: Var,
        offset: Var,
        index: Scalar,
    },
}

/// A rule: what to look for, and what it proves.
#[derive(Clone, Debug)]
pub struct Rule<L> {
    pub name: String,
    pub plan: Plan<L>,
    pub head: Vec<HeadOp<L>>,
    /// Variables the head binds, numbered above the query's. A match carries
    /// only what it matched; a head's own classes exist for as long as it runs.
    pub head_vars: u32,
    /// Applied once after the iterative fixpoint rather than in it, for a law
    /// whose right-hand side would feed back through the others.
    pub post_saturation: bool,
}

/// `head` as generic writes, appended to `out`. The operands of a commutative
/// operator are sorted once canonical: both of a binary one as written, all
/// of a filled one.
pub(crate) fn compile<L: Label>(head: &[HeadOp<L>], out: &mut Vec<Head>) {
    out.extend(head.iter().map(|op| match op {
        HeadOp::Insert { label, args, into } => {
            let template = &label.template;
            debug_assert_eq!(template.children().len(), args.len());
            Head::Insert {
                args: args.clone(),
                sorted: template.commutative() && (!label.fills.is_empty() || args.len() == 2),
                into: *into,
            }
        }
        HeadOp::Union(a, b) => Head::Equate(*a, *b),
        HeadOp::Offset { base, offset, into } => Head::Offset {
            base: *base,
            offset: offset.clone(),
            into: *into,
        },
        HeadOp::UnionIndexed {
            class,
            offset,
            index,
        } => Head::EquateIndexed {
            var: *class,
            offset: *offset,
            index: *index,
        },
    }));
}

/// The fills of `label` with the words a match bound.
fn fills<L>(label: &LabelFill<L>, scalars: &[u64]) -> SmallVec<[(Field, u64); 2]> {
    let fills = label.fills.iter();
    fills
        .map(|&(field, slot)| (field, scalars[slot as usize]))
        .collect()
}

fn label_fill<L>(op: &HeadOp<L>) -> &LabelFill<L> {
    match op {
        HeadOp::Insert { label, .. } => label,
        _ => unreachable!("only an insert has a key the client fills"),
    }
}

impl<L: Label> Engine<L> {
    /// Run `head` on one match. A head that cannot be spelled — a fill the
    /// language has no term for — writes nothing; everything before it stands,
    /// which is sound because a head only ever adds.
    pub fn apply_head(&mut self, head: &[HeadOp<L>], head_vars: u32, matched: &Match) {
        let mut compiled = Vec::new();
        compile(head, &mut compiled);
        let bindings: SmallVec<[tir_relational::Ref; 8]> = matched
            .bindings
            .iter()
            .map(|&binding| binding.into())
            .collect();
        let mut bound = std::mem::take(&mut self.head_bound);
        let mut run = Run {
            eg: self,
            rules: &[],
            externs: &NoExterns,
            starts: vec![0],
            ops: head.iter().map(|op| (op, None)).collect(),
        };
        let scalars = &matched.scalars;
        rel::apply(
            &mut run, CLASSES, 0, &compiled, head_vars, &bindings, scalars, &mut bound,
        );
        self.head_bound = bound;
    }

    /// What insert `op` over `operands`, which are canonical, comes to before
    /// a table is probed. A label as written is remembered in `cached`, so a
    /// node the graph already holds costs a key lookup and no term is built
    /// for it; a filled one is spelled from the match's scalars, `None` when
    /// the language cannot spell it. A carrier's integer constant is a
    /// reference and interns nothing.
    #[inline]
    pub(crate) fn head_key(
        &mut self,
        op: &HeadOp<L>,
        cached: &mut Option<LabelId>,
        operands: &[tir_relational::Ref],
        scalars: &[u64],
        key: &mut Key,
    ) -> Option<Keyed> {
        let label = label_fill(op);
        let template = &label.template;
        let id = if label.fills.is_empty() {
            let id = match *cached {
                Some(id) => id,
                None => *cached.insert(self.intern(template)),
            };
            if self.is_int_constant(id)
                && let Some(found) = self.int_constant_ref(template)
            {
                return Some(Keyed::Is(found.into()));
            }
            id
        } else {
            let node = L::fill(template, &fills(label, scalars))?;
            debug_assert_eq!(node.commutative(), template.commutative());
            if let Some(found) = self.int_constant_ref(&node) {
                return Some(Keyed::Is(found.into()));
            }
            self.intern(&node)
        };
        let operands: SmallVec<[Ref; 4]> = operands.iter().map(|&o| o.into()).collect();
        Some(match self.shape(id, &operands, key) {
            Shaped::Ref(found) => Keyed::Is(found.into()),
            Shaped::Key(shift) => Keyed::At {
                table: self.label_slot(id),
                shift,
            },
        })
    }

    /// Mint the class of insert `op` at `key`, its label and then its
    /// operands. The node is the head's own spelling with those children, not
    /// the label's stored node, which carries whatever provenance the label was
    /// first met with.
    pub(crate) fn head_mint(
        &mut self,
        op: &HeadOp<L>,
        table: u32,
        key: &Key,
        scalars: &[u64],
    ) -> u32 {
        let label = label_fill(op);
        let mut node = match label.fills.is_empty() {
            true => label.template.clone(),
            false => {
                L::fill(&label.template, &fills(label, scalars)).expect("spelled for its label")
            }
        };
        for (child, &class) in node.children_mut().iter_mut().zip(&key.cells[1..]) {
            *child = ClassId(class);
        }
        let id = LabelId(key.cells[0]);
        debug_assert_eq!(self.label_slot(id), table);
        self.make_class(node, id, &key.cells, key.weights(), false)
            .0
    }
}

/// The e-graph running the heads of a rule set.
pub(super) struct Run<'a, L: Label> {
    pub(super) eg: &'a mut Engine<L>,
    pub(super) rules: &'a [Rule<L>],
    pub(super) externs: &'a dyn Externs<L>,
    /// Per rule, where its head ops start in `ops`.
    pub(super) starts: Vec<usize>,
    /// Every head op of the rule set, and the label it last interned.
    pub(super) ops: Vec<(&'a HeadOp<L>, Option<LabelId>)>,
}

impl<L: Label> Client<Lists, 1> for Run<'_, L> {
    fn db(&self) -> &Database<Lists, 1> {
        &self.eg.db
    }

    fn db_mut(&mut self) -> &mut Database<Lists, 1> {
        &mut self.eg.db
    }

    #[inline]
    fn key(
        &mut self,
        rule: usize,
        op: usize,
        operands: &[tir_relational::Ref],
        scalars: &[u64],
        key: &mut Key,
    ) -> Option<Keyed> {
        let (op, cached) = &mut self.ops[self.starts[rule] + op];
        self.eg.head_key(op, cached, operands, scalars, key)
    }

    fn mint(&mut self, rule: usize, op: usize, table: u32, key: &Key, scalars: &[u64]) -> u32 {
        let (op, _) = self.ops[self.starts[rule] + op];
        self.eg.head_mint(op, table, key, scalars)
    }

    fn equate(&mut self, a: tir_relational::Ref, b: tir_relational::Ref) {
        // A contradiction is recorded by the engine.
        let _ = self.eg.union(Ref::from(a), Ref::from(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Term;
    use crate::{Atom, NoExterns, Query};

    /// `f(?1, ?2)` proves `f(?1, ?2) = g(?1)`.
    fn rule() -> Rule<Term> {
        Rule {
            name: "f-to-g".into(),
            plan: Plan::compile(Query::tree(
                4,
                0,
                vec![Atom::Node {
                    template: Term::op("f", &[ClassId(0), ClassId(0)]),
                    args: SmallVec::from_slice(&[1, 2]),
                    class: 0,
                    row: None,
                }],
            )),
            head: vec![
                HeadOp::Insert {
                    label: LabelFill::plain(Term::op("g", &[ClassId(0)])),
                    args: SmallVec::from_slice(&[1]),
                    into: 3,
                },
                HeadOp::Union(0, 3),
            ],
            head_vars: 0,
            post_saturation: false,
        }
    }

    /// A caller may union and saturate without a rebuild in between: the first
    /// round still matches through the class the union absorbed.
    #[test]
    fn saturation_matches_through_a_union_nobody_rebuilt_for() {
        let mut eg = Engine::new();
        // The older class survives a union, so the `f` row names the absorbed one.
        let older = eg.add(Term::leaf("older")).class;
        let x = eg.add(Term::leaf("x")).class;
        let y = eg.add(Term::leaf("y")).class;
        let g = eg.add(Term::op("g", &[x])).class;
        let f = eg.add(Term::op("f", &[g, y])).class;
        eg.rebuild();
        eg.union(older, g).unwrap();

        // `f(g(?3), ?2)` proves `f(g(?3), ?2) = ?2`.
        let atom = |op: &str, children: usize, args: &[u32], class| Atom::Node {
            template: Term::op(op, &vec![ClassId(0); children]),
            args: SmallVec::from_slice(args),
            class,
            row: None,
        };
        let rule = Rule {
            name: "f-of-g".into(),
            plan: Plan::compile(Query::tree(
                4,
                0,
                vec![atom("g", 1, &[3], 1), atom("f", 2, &[1, 2], 0)],
            )),
            head: vec![HeadOp::Union(0, 2)],
            head_vars: 0,
            post_saturation: false,
        };
        eg.saturate_rules(&[rule], &NoExterns, 1, usize::MAX);
        assert!(eg.connected(f, y));
    }

    #[test]
    fn a_head_inserts_and_unions() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a")).class;
        let b = eg.add(Term::leaf("b")).class;
        let f = eg.add(Term::op("f", &[a, b])).class;
        eg.rebuild();

        let rule = rule();
        let found = rule.plan.search(&eg, [f], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        for matched in &found {
            eg.apply_head(&rule.head, rule.head_vars, matched);
        }
        eg.rebuild();

        let g = eg.add(Term::op("g", &[a])).class;
        assert!(eg.connected(f, g));
    }
}
