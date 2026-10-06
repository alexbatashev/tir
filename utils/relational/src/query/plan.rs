//! Evaluation orders: a query's atoms and guards placed once, from the root
//! and from each atom a semi-naive round can start at.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use smallvec::SmallVec;

use super::ir::{Atom, Lattice, Out, Param, Query, Scalar, Select, Var};
use crate::db::LatticeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Atom(usize),
    /// A negated sub-conjunction, by index into the plan's nested plans.
    Not(usize),
    /// A sideways row atom: its value is not bound, but its argument `slot`
    /// is, so it is reached through the rows holding that id rather than
    /// through its value.
    Parents {
        atom: usize,
        slot: u8,
    },
    Guard(usize),
}

/// An evaluation order: the steps, the row levels below the root, and the
/// plans of the negated conjunctions.
type Ordered = (Vec<Step>, usize, Vec<Plan>);

/// A query with its atoms ordered for evaluation: each step's key variable is
/// bound by the root or by an earlier step, so evaluation is a loop nest with
/// no search over orders. Guards run as soon as their inputs are bound.
#[derive(Clone, Debug)]
pub struct Plan {
    pub(super) query: Query,
    pub(super) steps: Vec<Step>,
    height: usize,
    /// One compiled plan per negated sub-conjunction, in the query's order.
    pub(super) nots: Vec<Plan>,
    /// Per atom, what the evaluator reads of it.
    pub(super) shapes: Vec<Shape>,
    /// One evaluation order per row atom, starting at that atom rather than at
    /// the root: what a round uses to search from the rows the round before
    /// wrote. Empty for a plan that cannot be searched that way. Worked out on
    /// first use: a plan compiled and never saturated with does not pay for it.
    anchors: OnceLock<Vec<Anchor>>,
    /// One evaluation order per lattice atom, starting at its key: what a
    /// round uses to search from the values the round before raised. Empty for
    /// a plan that cannot be searched that way.
    fact_anchors: OnceLock<Vec<FactAnchor>>,
    /// Names the plan to a client's caches. Zero for a plan that is part of
    /// another.
    id: u64,
}

/// An atom as the evaluator reads it, worked out once rather than per row: a
/// row atom's selection, tag, swap set, key and arguments, each argument with
/// the column it sits in as written and as read swapped.
#[derive(Clone, Debug)]
pub(super) enum Shape {
    Rows(RowShape),
    Lattice(Lattice),
}

#[derive(Clone, Debug)]
pub(super) struct RowShape {
    pub(super) select: Select,
    pub(super) tag: Option<Scalar>,
    /// The parameter naming the cells whose rows also read swapped.
    pub(super) when: Option<Param>,
    pub(super) key: Var,
    pub(super) args: SmallVec<[Arg; 4]>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Arg {
    pub(super) column: u8,
    pub(super) swapped: u8,
    pub(super) var: Var,
}

impl Shape {
    fn of(atom: &Atom) -> Self {
        match atom {
            Atom::Rows(rows) => Shape::Rows(RowShape {
                select: rows.select,
                tag: rows.tag,
                when: rows.swap.map(|swap| swap.when),
                key: rows.value(),
                args: rows
                    .args()
                    .map(|(column, var)| Arg {
                        column: column as u8,
                        swapped: rows.swapped(column) as u8,
                        var,
                    })
                    .collect(),
            }),
            Atom::Lattice(lattice) => Shape::Lattice(lattice.clone()),
        }
    }
}

/// An evaluation order that starts at one row atom. Its first step is that
/// atom, which the search scans; the rest reach every other atom from what it
/// bound.
#[derive(Clone, Debug)]
pub(super) struct Anchor {
    pub(super) atom: usize,
    pub(super) steps: Vec<Step>,
}

/// An evaluation order that starts at the key a lattice atom reads: its first
/// step is that atom.
#[derive(Clone, Debug)]
pub(super) struct FactAnchor {
    pub(super) column: LatticeId,
    pub(super) key: Var,
    pub(super) steps: Vec<Step>,
    /// The lattice atoms that come before this one, as a mask over atom
    /// indices. A match with several new values is found from the first of
    /// them, so from this anchor those must be old.
    pub(super) earlier: u64,
    /// Whether no earlier lattice atom reads the same key. An id minted with
    /// its values has all of them new at once, so only such an anchor can find
    /// a match there.
    pub(super) first_on_key: bool,
}

impl Plan {
    /// Order `query`'s atoms — the root's atom first, then whatever it bound,
    /// in the order the query was written — with each guard placed at the
    /// first point its inputs are all bound.
    ///
    /// Panics if an atom's key is unreachable from the root, or a guard reads
    /// a scalar nothing binds.
    pub fn compile(query: Query) -> Self {
        let mut bound = vec![false; query.vars as usize];
        bound[query.root as usize] = true;
        let known = vec![false; query.scalars as usize];
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut plan = Self::compile_from(query, bound, known, None)
            .expect("every atom is reached and every guard read");
        plan.id = NEXT.fetch_add(1, Ordering::Relaxed);
        plan
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn anchors(&self) -> &[Anchor] {
        self.anchors.get_or_init(|| self.anchor())
    }

    /// Whether a round can search this plan from the rows the round before
    /// wrote, instead of from every id near a change.
    pub fn anchorable(&self) -> bool {
        !self.anchors().is_empty()
    }

    /// An evaluation order from each row atom. Empty for a plan a round cannot
    /// search from its new rows: one that negates, whose root binds no row, or
    /// some atom of which cannot be reached from another.
    fn anchor(&self) -> Vec<Anchor> {
        let query = &self.query;
        let rooted = query
            .atoms
            .iter()
            .any(|atom| matches!(atom, Atom::Rows(rows) if rows.value() == query.root));
        if !rooted || !self.nots.is_empty() {
            return Vec::new();
        }
        let mut anchors = Vec::new();
        for (index, atom) in query.atoms.iter().enumerate() {
            let Atom::Rows(rows) = atom else {
                continue;
            };
            let mut bound = vec![false; query.vars as usize];
            bound[rows.value() as usize] = true;
            for (_, arg) in rows.args() {
                bound[arg as usize] = true;
            }
            let mut known = vec![false; query.scalars as usize];
            if let Some(tag) = rows.tag {
                known[tag as usize] = true;
            }
            let Some((rest, _, _)) = Self::order(query, bound, known, Some(index)) else {
                return Vec::new();
            };
            let mut steps = vec![Step::Atom(index)];
            steps.extend(rest);
            anchors.push(Anchor { atom: index, steps });
        }
        anchors
    }

    /// A row atom's selection the host can tell, without resolving anything,
    /// reads no table, so the plan has no match.
    pub fn blocked<H: super::Host + ?Sized>(&self, host: &H) -> Option<super::Param> {
        self.query.atoms.iter().find_map(|atom| match atom {
            Atom::Rows(rows) if host.empty(rows.select.param) => Some(rows.select.param),
            _ => None,
        })
    }

    /// Whether the plan reads no row at all: every atom is a lattice atom.
    pub fn rowless(&self) -> bool {
        !self
            .query
            .atoms
            .iter()
            .any(|atom| matches!(atom, Atom::Rows(_)))
    }

    /// Whether a round can search this plan from the values the round before
    /// raised.
    pub fn fact_anchorable(&self) -> bool {
        !self.fact_anchors().is_empty()
    }

    pub(super) fn fact_anchors(&self) -> &[FactAnchor] {
        self.fact_anchors.get_or_init(|| self.fact_anchor())
    }

    /// An evaluation order from each lattice atom. Empty for a plan that
    /// negates, reads no lattice column, or some atom of which cannot be
    /// reached from one.
    fn fact_anchor(&self) -> Vec<FactAnchor> {
        let query = &self.query;
        if !self.nots.is_empty() {
            return Vec::new();
        }
        let mut facts: Vec<(usize, LatticeId, Var)> = query
            .atoms
            .iter()
            .enumerate()
            .filter_map(|(index, atom)| match atom {
                Atom::Lattice(lattice) => Some((index, lattice.column, lattice.key)),
                Atom::Rows(_) => None,
            })
            .collect();
        if facts.iter().any(|&(index, _, _)| index >= 64) {
            return Vec::new();
        }
        // Columns are tried in the order they were declared, which a client
        // makes the order of how rarely they rise.
        facts.sort_by_key(|&(index, column, _)| (column.index(), index));
        let mut anchors = Vec::new();
        let mut earlier = 0u64;
        for (position, &(index, column, key)) in facts.iter().enumerate() {
            let mut bound = vec![false; query.vars as usize];
            bound[key as usize] = true;
            let known = vec![false; query.scalars as usize];
            // No atom is skipped, but the order is not from the root.
            let Some((steps, _, _)) = Self::order(query, bound, known, Some(usize::MAX)) else {
                return Vec::new();
            };
            let first_on_key = !facts[..position].iter().any(|&(_, _, other)| other == key);
            anchors.push(FactAnchor {
                column,
                key,
                steps,
                earlier,
                first_on_key,
            });
            earlier |= 1 << index;
        }
        anchors
    }

    /// The lattice columns this plan reads, as a mask over their ids. A match
    /// can be new without any of its rows being new when a value rises under
    /// rows that stand still.
    pub fn fact_columns(&self) -> u64 {
        self.query.atoms.iter().fold(0, |mask, atom| match atom {
            Atom::Lattice(lattice) => mask | 1 << lattice.column.index(),
            Atom::Rows(_) => mask,
        })
    }

    fn compile_from(
        query: Query,
        bound: Vec<bool>,
        known: Vec<bool>,
        skip: Option<usize>,
    ) -> Option<Self> {
        let (steps, height, nots) = Self::order(&query, bound, known, skip)?;
        let shapes = query.atoms.iter().map(Shape::of).collect();
        Some(Self {
            query,
            steps,
            height,
            nots,
            shapes,
            anchors: OnceLock::new(),
            fact_anchors: OnceLock::new(),
            id: 0,
        })
    }

    /// Order what `bound` and `known` do not already give: the steps, the row
    /// levels below the root, and the negated conjunctions' plans. `None` when
    /// an atom is reachable neither through its bound key nor sideways from a
    /// bound argument, or a guard reads a scalar nothing binds.
    ///
    /// `skip` says the order does not start at the root, and names the atom the
    /// caller has matched already when there is one.
    fn order(
        query: &Query,
        mut bound: Vec<bool>,
        mut known: Vec<bool>,
        skip: Option<usize>,
    ) -> Option<Ordered> {
        let mut steps = Vec::with_capacity(query.atoms.len() + query.guards.len());
        let mut taken = vec![false; query.atoms.len()];
        if let Some(taken) = skip.and_then(|atom| taken.get_mut(atom)) {
            *taken = true;
        }
        let mut checked = vec![false; query.guards.len()];
        let mut nots: Vec<Plan> = Vec::new();
        let mut placed = vec![false; query.nots.len()];
        let mut depth = vec![0usize; query.vars as usize];
        let mut height = 0usize;
        loop {
            // A host call that binds nothing may be an assertion about a whole
            // match rather than a filter, and a rule's author sees to it that
            // every filter runs first when the match is built from the root. An
            // order that starts elsewhere keeps that promise by running such a
            // call only once every atom is matched.
            let all_taken = taken.iter().all(|&t| t);
            let guard = (0..query.guards.len()).find(|&i| {
                let held_back = skip.is_some()
                    && !all_taken
                    && matches!(&query.guards[i], super::Guard::Extern { out, .. } if out.is_empty());
                !checked[i]
                    && !held_back
                    && query.guards[i].reads().iter().all(|&s| known[s as usize])
                    && query.guards[i].vars().iter().all(|&v| bound[v as usize])
            });
            if let Some(guard) = guard {
                checked[guard] = true;
                for out in query.guards[guard].writes() {
                    known[out as usize] = true;
                }
                steps.push(Step::Guard(guard));
                continue;
            }
            // A negated conjunction reads and never writes, so it goes as soon
            // as everything it names is bound — the earliest it can prune.
            let not = (0..query.nots.len()).find_map(|i| {
                if placed[i] {
                    return None;
                }
                let nested = Query {
                    space: query.space,
                    vars: query.vars,
                    scalars: query.scalars,
                    params: query.params,
                    root: query.root,
                    atoms: query.nots[i].atoms.clone(),
                    guards: query.nots[i].guards.clone(),
                    nots: Vec::new(),
                };
                Plan::compile_from(nested, bound.clone(), known.clone(), None).map(|plan| (i, plan))
            });
            if let Some((index, plan)) = not {
                placed[index] = true;
                steps.push(Step::Not(nots.len()));
                nots.push(plan);
                continue;
            }
            if taken.iter().all(|&t| t) {
                break;
            }
            let downward =
                (0..query.atoms.len()).find(|&i| !taken[i] && bound[query.atoms[i].key() as usize]);
            // Nothing left to reach through a bound key: reach a row atom
            // whose value is still free through an argument that is not,
            // which is the one shape a root-first plan cannot cover.
            let (next, step) = match downward {
                Some(next) => (next, Step::Atom(next)),
                None => {
                    let (next, slot) =
                        (0..query.atoms.len())
                            .filter(|&i| !taken[i])
                            .find_map(|i| match &query.atoms[i] {
                                Atom::Rows(rows) => rows
                                    .args()
                                    .position(|(_, arg)| bound[arg as usize])
                                    .map(|slot| (i, slot as u8)),
                                Atom::Lattice(_) => None,
                            })?;
                    (next, Step::Parents { atom: next, slot })
                }
            };
            taken[next] = true;
            steps.push(step);
            bound[query.atoms[next].key() as usize] = true;
            let below = depth[query.atoms[next].key() as usize] + 1;
            match &query.atoms[next] {
                Atom::Rows(rows) => {
                    height = height.max(below);
                    for (_, arg) in rows.args() {
                        if !bound[arg as usize] {
                            bound[arg as usize] = true;
                            depth[arg as usize] = below;
                        }
                    }
                }
                Atom::Lattice(lattice) => {
                    for out in &lattice.out {
                        if let Out::Var(var) = out {
                            bound[*var as usize] = true;
                        }
                    }
                }
            }
            for slot in query.atoms[next].writes() {
                known[slot as usize] = true;
            }
        }
        (checked.iter().all(|&c| c) && placed.iter().all(|&p| p)).then_some((steps, height, nots))
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The atoms and guards in evaluation order.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Row levels below the root this plan binds. An id outside the frontier
    /// at this depth has an unchanged cone down to it, so its matches are the
    /// previous round's.
    ///
    /// Only meaningful for a plan that walks down: see [`Self::unbounded`].
    pub fn height(&self) -> usize {
        self.height
    }

    /// Whether the match depends on rows the root's downward cone does not
    /// contain, so neither narrowing a round's roots to the change frontier nor
    /// skipping a match with no new row of its own is licensed.
    ///
    /// Two shapes do this. A sideways atom sits under a sibling sharing an
    /// argument, which no upward closure of the change log reaches from the
    /// root. And a negated conjunction is read against the whole database: what
    /// satisfies it can change without anything in the match moving at all.
    pub fn unbounded(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, Step::Parents { .. } | Step::Not(_)))
            || self.nots.iter().any(Plan::unbounded)
    }
}
