//! Rules as conjunctive queries over the e-graph: a rule's left-hand side is a
//! set of atoms over e-nodes and the facts known of classes, and matching it is
//! a join rather than a backtracking walk over a goal stack.
//!
//! A [`Query`] is the rule IR rules are written in; a [`Plan`] compiles it onto
//! the database's query IR, one generic atom per atom written, and
//! [`Plan::search`] runs it. Each label is a table's selected cell, so a
//! template reads the labels it matches, and a commutative label lets a binary
//! atom match its row swapped. The order a plan produces matches in is part of
//! the contract: heads are applied in match order, so the ids a saturation
//! mints depend on it.

use smallvec::SmallVec;

use super::engine::{CLASSES, OBJECTS, lattice};
use crate::query::{self as rel, Cmp, Expr, ExternId, Host, Out, Param, Resolution, Scalar, Var};
use crate::{ClassId, Engine, Label, LabelId, RowId};

/// A field of a label the language lets a rule read or fill.
pub type Field = u32;

/// A lattice column of the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnId {
    /// The constant a class is known to be.
    Const,
    /// The type its terms carry.
    Type,
    /// The class it is derived from and the distance to it.
    Object,
}

/// The host's primitive functions over what a match bound: labels an atom
/// matched or a fact named, and words a guard computed. A guard never sees the
/// graph — a label is an e-node stripped of its children, so there is nowhere to
/// navigate from it — which is what makes a match's existence a function of its
/// atoms, and so what makes the delta exact.
pub trait Externs<L> {
    /// Fill `out` and report success; failure fails the match.
    fn call(&self, id: ExternId, labels: &[&L], args: &[u64], out: &mut [u64]) -> bool;
}

/// No externs: every call fails.
pub struct NoExterns;

impl<L> Externs<L> for NoExterns {
    fn call(&self, _id: ExternId, _labels: &[&L], _args: &[u64], _out: &mut [u64]) -> bool {
        false
    }
}

/// A term a guard reads: the e-node a row atom matched, or the label a fact
/// named. A row keeps everything the node was interned with — its provenance,
/// which label identity drops — so the two are not interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Row(Scalar),
    Label(Scalar),
}

impl Source {
    fn slot(self) -> Scalar {
        match self {
            Source::Row(slot) | Source::Label(slot) => slot,
        }
    }

    fn kind(self) -> Kind {
        match self {
            Source::Row(_) => Kind::Row,
            Source::Label(_) => Kind::Label,
        }
    }
}

/// A total or partial predicate over bound scalars.
#[derive(Clone, Debug)]
pub enum Guard {
    Cmp(Cmp, Expr, Expr),
    /// Bind `out` to what `value` evaluates to.
    Let {
        out: Scalar,
        value: Expr,
    },
    /// The pairs are not all bound to the same class. One pair is plain
    /// disequality; several say "these two terms are not the same term".
    Distinct(SmallVec<[(Var, Var); 4]>),
    /// Read `field` off the label a fact column bound, so a constant's value and
    /// width reach the guards as words. The engine owns the label table; this is
    /// a decode, not a look at the graph.
    Read {
        term: Source,
        field: Field,
        out: Scalar,
    },
    /// A host function over the labels and words named by `labels` and `args`;
    /// it binds `out`, and failing fails the match.
    Extern {
        call: ExternId,
        terms: SmallVec<[Source; 2]>,
        args: SmallVec<[Expr; 4]>,
        out: SmallVec<[Scalar; 2]>,
    },
}

#[derive(Clone, Debug)]
pub enum Atom<L> {
    /// A row of `class` whose label matches `template` and whose children bind
    /// `args`, one per operand. `row` binds the matched row, whose e-node a
    /// guard reads fields off or hands to a host function.
    Node {
        template: L,
        args: SmallVec<[Var; 4]>,
        class: Var,
        row: Option<Scalar>,
    },
    /// `class` holds `value` as a childless row, or is assumed to evaluate to it.
    Literal { value: L, class: Var },
    /// `key` has a value in `column`; bind it to `value`.
    Fact {
        column: ColumnId,
        key: Var,
        value: Scalar,
    },
    /// What `key` is derived from: `base` and the distance `offset`. A class
    /// nothing derived is its own base at zero — the reading that makes two
    /// unrelated addresses overlap rather than not — and a class whose
    /// derivations disagree satisfies nothing.
    Object { key: Var, base: Var, offset: Scalar },
}

/// A negated sub-conjunction: no solution of it exists, given what the outer
/// match bound. See [`rel::Nested`].
#[derive(Clone, Debug)]
pub struct Nested<L> {
    pub atoms: Vec<Atom<L>>,
    pub guards: Vec<Guard>,
}

impl<L> Atom<L> {
    /// The variable this atom's rows are looked up under; bound before the atom
    /// is stepped.
    pub fn class(&self) -> Var {
        match self {
            Atom::Node { class, .. } | Atom::Literal { class, .. } => *class,
            Atom::Fact { key, .. } | Atom::Object { key, .. } => *key,
        }
    }
}

/// A conjunctive query. Every atom's class variable is reachable from
/// [`Self::root`] through the atoms' `args`, which is what lets a plan bind
/// them in one downward pass.
#[derive(Clone, Debug)]
pub struct Query<L> {
    pub vars: u32,
    pub scalars: u32,
    pub root: Var,
    pub atoms: Vec<Atom<L>>,
    pub guards: Vec<Guard>,
    /// Sub-conjunctions that must have no solution. Placed once everything they
    /// read is bound, and so last.
    pub nots: Vec<Nested<L>>,
}

impl<L> Query<L> {
    /// A query with no scalars and no guards: a bare structural pattern.
    pub fn tree(vars: u32, root: Var, atoms: Vec<Atom<L>>) -> Self {
        Self {
            vars,
            scalars: 0,
            root,
            atoms,
            guards: Vec::new(),
            nots: Vec::new(),
        }
    }
}

/// One match: the class the root variable bound to, and the class every
/// variable bound to. `None` for a variable no atom reached.
#[derive(Clone, Debug)]
pub struct Match {
    pub root: ClassId,
    pub bindings: SmallVec<[Option<ClassId>; 8]>,
    pub scalars: SmallVec<[u64; 8]>,
}

/// A query compiled onto the database's query IR, with what its parameters
/// and host calls mean in e-graph terms.
#[derive(Clone, Debug)]
pub struct Plan<L> {
    query: Query<L>,
    plan: rel::Plan,
    params: Vec<ParamSpec<L>>,
    calls: Vec<Call>,
    /// The operator the root atom binds, or `None` when the root binds no row.
    root_op: Option<u64>,
}

/// What a parameter of the compiled query resolves to.
#[derive(Clone, Debug)]
enum ParamSpec<L> {
    /// The labels in the `op` bucket that `template` matches at its arity, by
    /// the table their rows live in.
    Template { template: L, arity: usize, op: u64 },
    /// The labels of that template parameter that commute, by table.
    Commuting(Param),
    /// The labels in the `op` bucket that `value` matches: the constants a
    /// literal accepts.
    Equal { value: L, op: u64 },
}

/// What a host call of the compiled query decodes its words as.
#[derive(Clone, Debug)]
enum Call {
    /// Read `field` off the term the one argument names.
    Read { kind: Kind, field: Field },
    /// The user's extern, its first arguments naming terms of these kinds.
    User {
        call: ExternId,
        kinds: SmallVec<[Kind; 2]>,
    },
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Row,
    Label,
}

/// Collects what [`Plan::compile`] lowers to.
struct Lower<L> {
    params: Vec<ParamSpec<L>>,
    calls: Vec<Call>,
}

impl<L: Label> Lower<L> {
    fn param(&mut self, spec: ParamSpec<L>) -> Param {
        self.params.push(spec);
        self.params.len() as Param - 1
    }

    fn atom(&mut self, atom: &Atom<L>) -> rel::Atom {
        match atom {
            Atom::Node {
                template,
                args,
                class,
                row,
            } => {
                let arity = args.len();
                let op = template.op_key();
                let template = template.clone();
                let labels = self.param(ParamSpec::Template {
                    template,
                    arity,
                    op,
                });
                // Every binary atom may read a row swapped: which labels a
                // template matches decides which of them commute.
                let swap = (arity == 2).then(|| rel::Swap {
                    columns: [1, 2],
                    when: self.param(ParamSpec::Commuting(labels)),
                });
                let mut terms: SmallVec<[rel::Term; 6]> = SmallVec::new();
                terms.push(rel::Term::Any);
                terms.extend(args.iter().map(|&arg| rel::Term::Var(arg)));
                terms.push(rel::Term::Var(*class));
                rel::Atom::Rows(rel::Rows {
                    select: rel::Select {
                        column: 0,
                        param: labels,
                    },
                    terms,
                    tag: *row,
                    swap,
                })
            }
            Atom::Literal { value, class } => {
                let op = value.op_key();
                let value = value.clone();
                let param = self.param(ParamSpec::Equal { value, op });
                lattice_atom(ColumnId::Const, *class, [Out::In(param)])
            }
            Atom::Fact {
                column: ColumnId::Object,
                ..
            } => panic!("a derivation is not one word: read it with Atom::Object"),
            Atom::Fact { column, key, value } => lattice_atom(*column, *key, [Out::Word(*value)]),
            Atom::Object { key, base, offset } => rel::Atom::Lattice(rel::Lattice {
                column: OBJECTS,
                key: *key,
                out: SmallVec::from_slice(&[Out::Var(*base), Out::Word(*offset)]),
            }),
        }
    }

    fn guard(&mut self, guard: &Guard) -> rel::Guard {
        match guard {
            Guard::Cmp(cmp, a, b) => rel::Guard::Cmp(*cmp, a.clone(), b.clone()),
            Guard::Let { out, value } => rel::Guard::Let {
                out: *out,
                value: value.clone(),
            },
            Guard::Distinct(pairs) => rel::Guard::Distinct(pairs.clone()),
            // One extern with an output, as a read was: never held back.
            Guard::Read { term, field, out } => {
                self.calls.push(Call::Read {
                    kind: term.kind(),
                    field: *field,
                });
                rel::Guard::Extern {
                    call: self.calls.len() as ExternId - 1,
                    args: SmallVec::from_elem(Expr::Scalar(term.slot()), 1),
                    out: SmallVec::from_slice(&[*out]),
                }
            }
            Guard::Extern {
                call,
                terms,
                args,
                out,
            } => {
                self.calls.push(Call::User {
                    call: *call,
                    kinds: terms.iter().map(|term| term.kind()).collect(),
                });
                let words = terms.iter().map(|term| Expr::Scalar(term.slot()));
                rel::Guard::Extern {
                    call: self.calls.len() as ExternId - 1,
                    args: words.chain(args.iter().cloned()).collect(),
                    out: out.clone(),
                }
            }
        }
    }
}

fn lattice_atom<const N: usize>(column: ColumnId, key: Var, out: [Out; N]) -> rel::Atom {
    rel::Atom::Lattice(rel::Lattice {
        column: lattice(column),
        key,
        out: SmallVec::from_slice(&out),
    })
}

impl<L: Label> Plan<L> {
    /// Order `query`'s atoms — the root's atom first, then whatever its operands
    /// bound, in the order the query was written — with each guard placed at the
    /// first point its inputs are all bound.
    ///
    /// Panics if an atom's class variable is unreachable from the root, or a
    /// guard reads a scalar nothing binds.
    pub fn compile(query: Query<L>) -> Self {
        let mut lower = Lower {
            params: Vec::new(),
            calls: Vec::new(),
        };
        let atoms = query.atoms.iter().map(|atom| lower.atom(atom)).collect();
        let guards = query
            .guards
            .iter()
            .map(|guard| lower.guard(guard))
            .collect();
        let nots = query
            .nots
            .iter()
            .map(|nested| rel::Nested {
                atoms: nested.atoms.iter().map(|atom| lower.atom(atom)).collect(),
                guards: nested
                    .guards
                    .iter()
                    .map(|guard| lower.guard(guard))
                    .collect(),
            })
            .collect();
        let root_op = match query.atoms.iter().find(|atom| atom.class() == query.root) {
            Some(Atom::Node { template, .. }) => Some(template.op_key()),
            _ => None,
        };
        let plan = rel::Plan::compile(rel::Query {
            space: CLASSES,
            vars: query.vars,
            scalars: query.scalars,
            params: lower.params.len() as u32,
            root: query.root,
            atoms,
            guards,
            nots,
        });
        Self {
            query,
            plan,
            params: lower.params,
            calls: lower.calls,
            root_op,
        }
    }

    /// The query as written.
    pub fn query(&self) -> &Query<L> {
        &self.query
    }

    /// The compiled plan.
    pub(crate) fn generic(&self) -> &rel::Plan {
        &self.plan
    }

    /// The atoms and guards in evaluation order; an atom or guard is named by
    /// its index in the query as written.
    pub fn steps(&self) -> &[rel::Step] {
        self.plan.steps()
    }

    /// See [`rel::Plan::unbounded`].
    pub fn unbounded(&self) -> bool {
        self.plan.unbounded()
    }

    /// The classes the root atom can match at: those holding its operator, or
    /// every class when the root binds no row.
    pub fn roots(&self, eg: &Engine<L>) -> Vec<ClassId> {
        match self.root_op {
            Some(op) => eg.classes_with_op(op),
            None => eg.class_ids().collect(),
        }
    }

    /// The operator the root atom binds, or `None` when the root binds no row.
    pub(crate) fn root_op(&self) -> Option<u64> {
        self.root_op
    }

    /// Every match at `roots`, each root canonicalized and visited once.
    ///
    /// `allowed(var, class)` prunes a binding the caller rejects — the hook for
    /// operand constraints instruction selection carries outside the query.
    /// When `only_new` is set, a match nothing of whose rows or facts the
    /// previous round touched is dropped: it existed a round earlier at a root
    /// that was searched then, so its head ran then.
    pub fn search(
        &self,
        eg: &Engine<L>,
        roots: impl IntoIterator<Item = ClassId>,
        allowed: &dyn Fn(Var, ClassId) -> bool,
        only_new: bool,
        externs: &dyn Externs<L>,
    ) -> Vec<Match> {
        let host = Lookup {
            eg,
            plan: self,
            externs,
        };
        let mut caches = eg.caches.borrow_mut();
        let Some(cache) = caches.plan(&self.plan, &host) else {
            return Vec::new();
        };
        let allowed = |var, class| allowed(var, ClassId(class));
        let roots = roots.into_iter().map(|root| root.0);
        let fresh = (only_new, 0);
        let found = self
            .plan
            .search_roots(&eg.db, &host, roots, Some(&allowed), fresh, cache);
        drop(caches);
        // The classes of every match in one pass, then each match's copied
        // out whole: a match spelled id by id is read back before the narrow
        // writes retire.
        let width = self.query.vars as usize;
        let classes: Vec<Option<ClassId>> =
            found.bindings().iter().map(|id| id.map(ClassId)).collect();
        (0..found.len())
            .map(|index| {
                let (root, _, scalars) = found.get(index);
                Match {
                    root: ClassId(root),
                    bindings: SmallVec::from_slice(&classes[index * width..(index + 1) * width]),
                    scalars: SmallVec::from_slice(scalars),
                }
            })
            .collect()
    }

    /// Extend `out` by what parameter `param` resolves to in `eg`, among the
    /// labels met since `since` labels.
    pub(crate) fn resolve(
        &self,
        eg: &Engine<L>,
        param: Param,
        since: Option<u64>,
        out: &mut Resolution,
    ) {
        let since = since.map(|since| since as usize);
        match &self.params[param as usize] {
            ParamSpec::Template {
                template,
                arity,
                op,
            } => eg.labels_matching(*op, (template, *arity), false, since, out),
            ParamSpec::Commuting(labels) => {
                let ParamSpec::Template {
                    template,
                    arity,
                    op,
                } = &self.params[*labels as usize]
                else {
                    unreachable!("commutativity is of a template's labels")
                };
                eg.labels_matching(*op, (template, *arity), true, since, out)
            }
            ParamSpec::Equal { value, op } => eg.labels_equal(*op, value, since, out),
        }
    }

    /// Run host call `id` over `args`, decoding the words that name terms.
    pub(crate) fn call(
        &self,
        eg: &Engine<L>,
        externs: &dyn Externs<L>,
        id: ExternId,
        args: &[u64],
        out: &mut [u64],
    ) -> bool {
        let term = |kind: Kind, word: u64| match kind {
            Kind::Row => Some(eg.node(RowId(word as u32))),
            Kind::Label => eg.label_node(LabelId(word as u32)),
        };
        match &self.calls[id as usize] {
            Call::Read { kind, field } => {
                let Some(value) = term(*kind, args[0]).and_then(|node| node.scalar(*field)) else {
                    return false;
                };
                out[0] = value;
                true
            }
            Call::User { call, kinds } => {
                let Some(terms): Option<SmallVec<[&L; 2]>> = kinds
                    .iter()
                    .zip(args)
                    .map(|(&kind, &word)| term(kind, word))
                    .collect()
                else {
                    return false;
                };
                externs.call(*call, &terms, &args[kinds.len()..], out)
            }
        }
    }
}

/// A plan's host: the engine's labels and the user's externs.
pub(crate) struct Lookup<'a, L: Label> {
    pub(crate) eg: &'a Engine<L>,
    pub(crate) plan: &'a Plan<L>,
    pub(crate) externs: &'a dyn Externs<L>,
}

impl<L: Label> Host for Lookup<'_, L> {
    /// Labels only accumulate, so what a parameter resolves to is fixed
    /// between two readings that agree on how many there are.
    fn version(&self) -> u64 {
        self.eg.labels_len() as u64
    }

    fn resolve(&self, param: Param, since: Option<u64>, out: &mut Resolution) {
        self.plan.resolve(self.eg, param, since, out)
    }

    fn call(&self, id: ExternId, args: &[u64], out: &mut [u64]) -> bool {
        self.plan.call(self.eg, self.externs, id, args, out)
    }

    /// A template whose operator the graph has never met matches nothing.
    fn empty(&self, param: Param) -> bool {
        match &self.plan.params[param as usize] {
            ParamSpec::Template { op, .. } => self.eg.labels_with_op(*op).is_empty(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Step;
    use crate::egraph::testing::Term;
    use proptest::prelude::*;

    fn node(template: Term, args: &[Var], class: Var) -> Atom<Term> {
        Atom::Node {
            template,
            args: args.iter().copied().collect(),
            class,
            row: None,
        }
    }

    /// `f(g(?1), ?2)` with the atoms written parent-last, so ordering has to do
    /// something.
    fn f_of_g() -> Query<Term> {
        Query::tree(
            4,
            0,
            vec![
                node(Term::op("g", &[ClassId(0)]), &[3], 1),
                node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 2], 0),
            ],
        )
    }

    #[test]
    fn plan_steps_the_root_atom_first_then_what_it_bound() {
        let plan = Plan::compile(f_of_g());
        assert_eq!(plan.steps(), &[Step::Atom(1), Step::Atom(0)]);
        assert_eq!(plan.generic().height(), 2);
    }

    #[test]
    #[should_panic(expected = "every atom is reached")]
    fn plan_rejects_an_atom_no_operand_reaches() {
        Plan::compile(Query::tree(2, 0, vec![node(Term::leaf("x"), &[], 1)]));
    }

    #[test]
    fn search_binds_every_variable_of_the_pattern() {
        let mut eg = Engine::new();
        let x = eg.add(Term::leaf("x"));
        let y = eg.add(Term::leaf("y"));
        let g = eg.add(Term::op("g", &[x]));
        let f = eg.add(Term::op("f", &[g, y]));
        let found = Plan::compile(f_of_g()).search(&eg, [f], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].bindings.as_slice(),
            &[Some(f), Some(g), Some(y), Some(x)]
        );
    }

    /// Every assignment of `query`'s variables to classes that satisfies every
    /// atom, found by trying all of them.
    fn brute_force(eg: &Engine<Term>, query: &Query<Term>) -> Vec<Vec<ClassId>> {
        let classes: Vec<ClassId> = eg.class_ids().collect();
        let mut out = Vec::new();
        let mut assignment = vec![ClassId(0); query.vars as usize];
        let total = classes.len().pow(query.vars);
        for mut code in 0..total {
            for slot in assignment.iter_mut() {
                *slot = classes[code % classes.len()];
                code /= classes.len();
            }
            if query.atoms.iter().all(|atom| holds(eg, atom, &assignment)) {
                out.push(assignment.clone());
            }
        }
        out
    }

    fn holds(eg: &Engine<Term>, atom: &Atom<Term>, assignment: &[ClassId]) -> bool {
        let class = assignment[atom.class() as usize];
        match atom {
            Atom::Literal { value, .. } => {
                eg.const_of(class).is_some_and(|known| value.matches(known))
            }
            Atom::Fact { column, .. } => eg.fact(*column, class).is_some(),
            // The offset lands in a scalar the naive oracle cannot see, so only
            // the object is checked here.
            Atom::Object { base, .. } => {
                eg.object_of(class).map(|(from, _)| from) == Some(assignment[*base as usize])
            }
            Atom::Node { template, args, .. } => eg.rows(class).any(|row| {
                let children: Vec<ClassId> = eg.children(row).iter().map(|&c| eg.find(c)).collect();
                if children.len() != args.len() || !template.matches_template(eg.node(row)) {
                    return false;
                }
                let want: Vec<ClassId> = args.iter().map(|&a| assignment[a as usize]).collect();
                children == want
                    || (eg.node(row).commutative()
                        && children.len() == 2
                        && children == [want[1], want[0]])
            }),
        }
    }

    /// A negated conjunction blocks a match whose blocker no round created —
    /// it is read against the whole relation, never a delta.
    #[test]
    fn a_negated_atom_blocks_a_match_whatever_round_made_it() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let blocked = eg.add(Term::op("f", &[a, b]));
        let free = eg.add(Term::op("f", &[b, a]));
        eg.add(Term::op("g", &[a]));
        eg.rebuild();

        let query = Query {
            vars: 4,
            scalars: 0,
            root: 0,
            atoms: vec![node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 2], 0)],
            guards: Vec::new(),
            nots: vec![Nested {
                atoms: vec![node(Term::op("g", &[ClassId(0)]), &[1], 3)],
                guards: Vec::new(),
            }],
        };
        let plan = Plan::compile(query);
        assert_eq!(plan.steps(), &[Step::Atom(0), Step::Not(0)]);
        let found = plan.search(&eg, [blocked, free], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, free);
    }

    /// A second root reached from an operand the first bound.
    #[test]
    fn a_sideways_atom_is_reached_through_a_bound_operand() {
        let mut eg = Engine::new();
        let a = eg.add(Term::leaf("a"));
        let b = eg.add(Term::leaf("b"));
        let c = eg.add(Term::leaf("c"));
        let f = eg.add(Term::op("f", &[a, b]));
        let g = eg.add(Term::op("g", &[a, b]));
        eg.add(Term::op("g", &[a, c]));
        eg.rebuild();

        let plan = Plan::compile(Query::tree(
            4,
            0,
            vec![
                node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 2], 0),
                node(Term::op("g", &[ClassId(0), ClassId(0)]), &[1, 2], 3),
            ],
        ));
        assert_eq!(
            plan.steps(),
            &[Step::Atom(0), Step::Parents { atom: 1, slot: 0 }]
        );
        let found = plan.search(&eg, [f], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bindings[3], Some(g));
    }

    /// `f(?1, ?2)` where `?1` is a literal below ten.
    #[test]
    fn a_guard_runs_as_soon_as_its_scalars_are_bound() {
        let mut eg = Engine::new();
        let small = eg.add(Term::int(3));
        let big = eg.add(Term::int(30));
        let y = eg.add(Term::leaf("y"));
        let hit = eg.add(Term::op("f", &[small, y]));
        let miss = eg.add(Term::op("f", &[big, y]));
        eg.rebuild();

        let query = Query {
            vars: 3,
            scalars: 2,
            root: 0,
            atoms: vec![
                node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 2], 0),
                Atom::Fact {
                    column: ColumnId::Const,
                    key: 1,
                    value: 0,
                },
            ],
            guards: vec![
                Guard::Read {
                    term: Source::Label(0),
                    field: 0,
                    out: 1,
                },
                Guard::Cmp(Cmp::Lt, Expr::Scalar(1), Expr::Lit(10)),
            ],
            nots: Vec::new(),
        };
        let plan = Plan::compile(query);
        assert_eq!(
            plan.steps(),
            &[Step::Atom(0), Step::Atom(1), Step::Guard(0), Step::Guard(1)]
        );
        let found = plan.search(&eg, [hit, miss], &|_, _| true, false, &NoExterns);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, hit);
        assert_eq!(found[0].scalars[1], 3);
    }

    /// A term over three leaves and three operators, one of them commutative.
    fn term_strategy() -> impl Strategy<Value = Vec<(String, Vec<usize>)>> {
        prop::collection::vec(
            (
                prop_oneof![
                    Just("x".to_string()),
                    Just("y".to_string()),
                    Just("f".to_string()),
                    Just("g".to_string()),
                    Just("h".to_string()),
                ],
                prop::collection::vec(0usize..6, 0..3),
            ),
            1..8,
        )
    }

    /// Build an engine from `recipe`: each entry names an operator and the
    /// earlier entries its operands, wrapped into range.
    fn build(recipe: &[(String, Vec<usize>)]) -> (Engine<Term>, Vec<ClassId>) {
        let mut eg = Engine::new();
        let mut made: Vec<ClassId> = Vec::new();
        for (op, operands) in recipe {
            let arity = match op.as_str() {
                "x" | "y" => 0,
                "g" => 1,
                _ => 2,
            };
            let children: Vec<ClassId> = (0..arity)
                .map(|i| {
                    made.get(operands.get(i).copied().unwrap_or(0) % made.len().max(1))
                        .copied()
                        .unwrap_or(ClassId(0))
                })
                .collect();
            if made.is_empty() && arity > 0 {
                continue;
            }
            let term = if op == "h" {
                Term::comm(op, &children)
            } else {
                Term::op(op, &children)
            };
            made.push(eg.add(term));
        }
        eg.rebuild();
        (eg, made)
    }

    proptest! {
        /// The evaluator's bindings are exactly the satisfying assignments.
        #[test]
        fn query_equals_brute_force(recipe in term_strategy(), which in 0usize..5) {
            let (eg, made) = build(&recipe);
            prop_assume!(!made.is_empty());
            let query = match which {
                0 => Query::tree(3, 0, vec![
                    node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 2], 0)]),
                1 => Query::tree(3, 0, vec![
                    node(Term::comm("h", &[ClassId(0), ClassId(0)]), &[1, 2], 0)]),
                // One variable in two operand slots: the match must agree on it.
                2 => Query::tree(2, 0, vec![
                    node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 1], 0)]),
                // A second root, reached sideways through a shared operand.
                3 => Query::tree(4, 0, vec![
                    node(Term::op("f", &[ClassId(0), ClassId(0)]), &[1, 2], 0),
                    node(Term::op("g", &[ClassId(0), ClassId(0)]), &[1, 2], 3)]),
                _ => f_of_g(),
            };
            let roots: Vec<ClassId> = eg.class_ids().collect();
            let found = Plan::compile(query.clone()).search(&eg, roots, &|_, _| true, false, &NoExterns);
            let mut got: Vec<Vec<ClassId>> = found
                .iter()
                .map(|m| m.bindings.iter().map(|b| b.expect("bound")).collect())
                .collect();
            got.sort();
            got.dedup();
            let mut want = brute_force(&eg, &query);
            want.sort();
            want.dedup();
            prop_assert_eq!(got, want);
        }
    }
}
