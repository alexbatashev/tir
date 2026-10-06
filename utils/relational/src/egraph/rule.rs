//! A rule as data: a query, and a head that only writes.
//!
//! Nothing in a head reads the graph and nothing outside an atom reads it
//! either, so a match exists in round `t` and not `t-1` exactly when one of its
//! rows or facts is from round `t-1`'s delta. That is what a hand-asserted
//! "this applier reads no deeper than its pattern" used to promise and nothing
//! checked.

use smallvec::SmallVec;

use super::engine::{CLASSES, Lists, OBJECTS};
use super::query::{Externs, Field, Match, NoExterns, Plan};
use crate::db::Database;
use crate::query::{Expr, Scalar, Var};
use crate::rule::{self as rel, Client, Head, Key, KeyOrder, Value};
use crate::{ClassId, Engine, Label, LabelId};

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

/// One write of a rule's right-hand side.
#[derive(Clone, Debug)]
pub enum HeadOp<L> {
    /// Hash-cons `label(args)` and bind its class to `into`.
    Insert {
        label: LabelFill<L>,
        args: SmallVec<[Var; 4]>,
        into: Var,
    },
    Union(Var, Var),
    /// Record that `key` is `base` plus `offset`.
    RaiseObject {
        key: Var,
        base: Var,
        offset: Expr,
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

/// `head` as generic writes, appended to `out`. An insert's key is its label
/// and then its children, in the table of its arity and uniqueness; a
/// commutative operator's children are sorted, after canonicalizing them for a
/// template as written and before for a filled one.
pub(crate) fn compile<L: Label>(head: &[HeadOp<L>], out: &mut Vec<Head>) {
    out.extend(head.iter().map(|op| match op {
        HeadOp::Insert { label, args, into } => {
            let template = &label.template;
            debug_assert_eq!(template.children().len(), args.len());
            let order = match (template.commutative(), label.fills.is_empty()) {
                (false, _) => KeyOrder::AsWritten,
                (true, true) => KeyOrder::Sorted,
                (true, false) => KeyOrder::SortedRaw,
            };
            let mut key = SmallVec::from_elem(Key::Client, 1);
            key.extend(args.iter().map(|&arg| Key::Var(arg)));
            Head::Insert {
                table: table_of(template),
                key,
                order,
                into: *into,
            }
        }
        HeadOp::Union(a, b) => Head::Equate(*a, *b),
        HeadOp::RaiseObject { key, base, offset } => Head::Raise {
            column: OBJECTS,
            key: *key,
            value: SmallVec::from_iter([Value::Var(*base), Value::Expr(offset.clone())]),
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

/// The table a node's rows live in: one keyed table per arity, and beside it
/// a bag for the labels that never hash-cons.
pub(crate) fn table_of<L: Label>(node: &L) -> u32 {
    2 * node.children().len() as u32 + u32::from(node.is_unique())
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
        let bindings: SmallVec<[Option<u32>; 8]> = matched
            .bindings
            .iter()
            .map(|id| id.map(|id| id.0))
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

    /// The label insert `op` builds, as a key cell. A label as written is
    /// remembered in `cached`, so a node the graph already holds costs a key
    /// lookup and no term is built for it; a filled one is spelled from the
    /// match's scalars, `None` when the language cannot spell it.
    #[inline]
    pub(crate) fn head_label(
        &mut self,
        op: &HeadOp<L>,
        cached: &mut Option<LabelId>,
        scalars: &[u64],
    ) -> Option<u32> {
        let label = label_fill(op);
        let template = &label.template;
        if label.fills.is_empty() {
            let id = match *cached {
                Some(id) => id,
                None => *cached.insert(self.intern(template)),
            };
            return Some(id.0);
        }
        let node = L::fill(template, &fills(label, scalars))?;
        debug_assert_eq!(node.commutative(), template.commutative());
        debug_assert_eq!(table_of(&node), table_of(template));
        Some(self.intern(&node).0)
    }

    /// Mint the class of insert `op` at `key`, its label and then its
    /// children. The node is the head's own spelling with those children, not
    /// the label's stored node, which carries whatever provenance the label was
    /// first met with.
    pub(crate) fn head_mint(
        &mut self,
        op: &HeadOp<L>,
        table: u32,
        key: &[u32],
        scalars: &[u64],
    ) -> u32 {
        let label = label_fill(op);
        let mut node = match label.fills.is_empty() {
            true => label.template.clone(),
            false => {
                L::fill(&label.template, &fills(label, scalars)).expect("spelled for its label")
            }
        };
        for (child, &class) in node.children_mut().iter_mut().zip(&key[1..]) {
            *child = ClassId(class);
        }
        let id = LabelId(key[0]);
        debug_assert_eq!(self.label_slot(id), table);
        self.make_class(node, id, key).0
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
    fn cell(&mut self, rule: usize, op: usize, scalars: &[u64]) -> Option<u32> {
        let (op, cached) = &mut self.ops[self.starts[rule] + op];
        if let Some(label) = *cached {
            return Some(label.0);
        }
        self.eg.head_label(op, cached, scalars)
    }

    fn mint(&mut self, rule: usize, op: usize, table: u32, key: &[u32], scalars: &[u64]) -> u32 {
        let (op, _) = self.ops[self.starts[rule] + op];
        self.eg.head_mint(op, table, key, scalars)
    }

    fn equate(&mut self, a: u32, b: u32) {
        self.eg.union(ClassId(a), ClassId(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egraph::testing::Term;
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

    /// `add(p, k)` with `k` a literal is `p` plus `k` — a rule whose head raises
    /// the column its own atoms read, so a chain of them reaches a fixpoint.
    fn derivation() -> Rule<Term> {
        Rule {
            name: "derivation".into(),
            plan: Plan::compile(Query {
                vars: 4,
                scalars: 3,
                root: 0,
                atoms: vec![
                    Atom::Node {
                        template: Term::op("add", &[ClassId(0), ClassId(0)]),
                        args: SmallVec::from_slice(&[1, 2]),
                        class: 0,
                        row: None,
                    },
                    Atom::Fact {
                        column: crate::ColumnId::Const,
                        key: 2,
                        value: 0,
                    },
                    Atom::Object {
                        key: 1,
                        base: 3,
                        offset: 1,
                    },
                ],
                guards: vec![crate::Guard::Read {
                    term: crate::Source::Label(0),
                    field: 0,
                    out: 2,
                }],
                nots: Vec::new(),
            }),
            head: vec![HeadOp::RaiseObject {
                key: 0,
                base: 3,
                offset: crate::Expr::Add(
                    Box::new(crate::Expr::Scalar(1)),
                    Box::new(crate::Expr::Scalar(2)),
                ),
            }],
            head_vars: 0,
            post_saturation: false,
        }
    }

    #[test]
    fn a_chain_of_derivations_reaches_one_base_and_one_offset() {
        let mut eg = Engine::new();
        let p = eg.add(Term::leaf("p"));
        let four = eg.add(Term::int(4));
        let first = eg.add(Term::op("add", &[p, four]));
        let second = eg.add(Term::op("add", &[first, four]));
        eg.rebuild();

        let rule = derivation();
        for _ in 0..2 {
            let found = rule.plan.search(
                &eg,
                eg.class_ids().collect::<Vec<_>>(),
                &|_, _| true,
                false,
                &NoExterns,
            );
            for matched in &found {
                eg.apply_head(&rule.head, rule.head_vars, matched);
            }
        }
        assert_eq!(eg.object_of(p), Some((p, 0)));
        assert_eq!(eg.object_of(first), Some((p, 4)));
        assert_eq!(eg.object_of(second), Some((p, 8)));
    }

    /// A chain placed before its own base was placed says the same thing once
    /// the base is: what the column joins is where the two land, not the step
    /// each was written with.
    #[test]
    fn a_derivation_written_early_agrees_with_the_one_written_late() {
        let mut eg = Engine::new();
        let p = eg.add(Term::leaf("p"));
        let first = eg.add(Term::leaf("q"));
        let second = eg.add(Term::leaf("r"));
        eg.rebuild();

        eg.raise_object(second, first, 4);
        eg.raise_object(first, p, 4);
        eg.raise_object(second, p, 8);
        assert_eq!(eg.object_of(second), Some((p, 8)));
    }

    /// The same fact raised again after its base was absorbed is the same fact.
    #[test]
    fn a_derivation_survives_its_base_being_merged_away() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let x = eg.add(Term::leaf("x"));
        eg.rebuild();

        eg.raise_object(x, b, 4);
        let survivor = eg.union(a, b);
        eg.rebuild();
        eg.raise_object(x, survivor, 4);
        assert_eq!(eg.object_of(x), Some((survivor, 4)));
    }

    #[test]
    fn two_derivations_that_disagree_leave_the_class_unplaceable() {
        let mut eg = Engine::new();
        let p = eg.add(Term::leaf("p"));
        let q = eg.add(Term::leaf("q"));
        eg.rebuild();
        assert!(eg.raise_object(p, q, 4));
        assert!(eg.raise_object(p, q, 8));
        assert_eq!(eg.object_of(p), None);
    }

    /// A caller may union and saturate without a rebuild in between: the first
    /// round still matches through the class the union absorbed.
    #[test]
    fn saturation_matches_through_a_union_nobody_rebuilt_for() {
        let mut eg = Engine::new();
        // The older class survives a union, so the `f` row names the absorbed one.
        let older = eg.add(Term::leaf("older"));
        let x = eg.add(Term::leaf("x"));
        let y = eg.add(Term::leaf("y"));
        let g = eg.add(Term::op("g", &[x]));
        let f = eg.add(Term::op("f", &[g, y]));
        eg.rebuild();
        eg.union(older, g);

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
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let f = eg.add(Term::op("f", &[a, b]));
        eg.rebuild();

        let rule = rule();
        let found = rule.plan.search(&eg, [f], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        for matched in &found {
            eg.apply_head(&rule.head, rule.head_vars, matched);
        }
        eg.rebuild();

        let g = eg.add(Term::op("g", &[a]));
        assert!(eg.connected(f, g));
    }
}
