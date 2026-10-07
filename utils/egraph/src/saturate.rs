//! Saturation of the e-graph: the database's semi-naive driver, with the
//! e-graph choosing where a rule is searched by the operator its root binds.

use tir_relational::query::Host;
use tir_relational::rule as rel;
use tir_relational::saturate::{Delta, Saturate, saturate};

use crate::engine::{CLASSES, Lists};
use crate::query::{Externs, Lookup, Plan};
use crate::rule::{Rule, Run, compile};
use crate::{ClassId, Engine, Label};

/// Δ_h grouped by operator, as `(op, row, class)` sorted: for each operator,
/// the classes holding it paired with the row each one enters that
/// operator's bucket at, which is what orders the group. One list per height,
/// grown with the frontier.
type OpGroups = Vec<Vec<(u64, u32, u32)>>;

/// The classes of Δ_height holding `op`, in the order the operator's row
/// bucket holds them: where a rule of that height rooted on that operator can
/// match anew.
///
/// Scanning the frontier once per depth beats materializing each rule's whole
/// bucket and filtering it, because a settled round's frontier is a handful of
/// classes while the bucket holds every term of that shape in the function.
/// The bucket is appended to as rows are minted, and a class enters it at its
/// first row of that operator, so ordering each group by that row reproduces
/// the bucket's order exactly — which is the root order a match's position in
/// the round is defined by, and so the order class ids are assigned in.
fn op_roots<'d, L: Label>(
    eg: &Engine<L>,
    delta: &'d mut Delta<OpGroups>,
    height: usize,
    op: u64,
) -> &'d [(u64, u32, u32)] {
    delta.at(&eg.db, CLASSES, height);
    let mut by_op = std::mem::take(&mut delta.client);
    while by_op.len() <= height {
        let mut groups: Vec<(u64, u32, u32)> = Vec::new();
        for &class in delta.level(by_op.len()) {
            let first = groups.len();
            for row in eg.rows(ClassId(class)) {
                let op = eg.row_op(row);
                match groups[first..].iter_mut().find(|(seen, _, _)| *seen == op) {
                    Some((_, at, _)) => *at = (*at).min(row.0),
                    None => groups.push((op, row.0, class)),
                }
            }
        }
        groups.sort_unstable();
        by_op.push(groups);
    }
    delta.client = by_op;
    let groups = &delta.client[height];
    let from = groups.partition_point(|&(other, _, _)| other < op);
    let to = groups.partition_point(|&(other, _, _)| other <= op);
    &groups[from..to]
}

/// The classes a semi-naive round searches `plan` at: the change frontier at the
/// plan's height, restricted to the operator its root binds.
fn round_roots<L: Label>(eg: &Engine<L>, plan: &Plan<L>, delta: &mut Delta<OpGroups>) -> Vec<u32> {
    match plan.root_op() {
        Some(op) => op_roots(eg, delta, plan.generic().height(), op)
            .iter()
            .map(|&(_, _, class)| class)
            .collect(),
        None => delta.at(&eg.db, CLASSES, plan.generic().height()).to_vec(),
    }
}

impl<L: Label> Saturate<Lists, 1> for Run<'_, L> {
    type Frontier = OpGroups;

    fn version(&self) -> u64 {
        self.eg.labels_len() as u64
    }

    fn host(&self, rule: usize) -> impl Host + '_ {
        Lookup {
            eg: self.eg,
            plan: &self.rules[rule].plan,
            externs: self.externs,
        }
    }

    fn head(&self, rule: usize) -> Vec<rel::Head> {
        let mut head = Vec::new();
        compile(&self.rules[rule].head, &mut head);
        head
    }

    fn roots(&mut self, rule: usize, frontier: Option<&mut Delta<OpGroups>>) -> Vec<u32> {
        let plan = &self.rules[rule].plan;
        match frontier {
            Some(delta) => round_roots(self.eg, plan, delta),
            None => plan
                .roots(self.eg)
                .into_iter()
                .map(|class| class.0)
                .collect(),
        }
    }

    fn rebuild(&mut self) {
        self.eg.rebuild();
    }

    fn size(&self) -> usize {
        self.eg.total_size()
    }
}

impl<L: Label> Engine<L> {
    /// Saturate in place with `rules`, and the host functions their guards
    /// call; see [`saturate`]. The fixpoint test counts classes, e-nodes and
    /// raised facts, and the node limit counts e-nodes.
    pub fn saturate_rules(
        &mut self,
        rules: &[Rule<L>],
        externs: &dyn Externs<L>,
        iter_limit: usize,
        node_limit: usize,
    ) {
        let mut starts = Vec::with_capacity(rules.len());
        let mut ops = Vec::with_capacity(rules.iter().map(|rule| rule.head.len()).sum());
        for rule in rules {
            starts.push(ops.len());
            ops.extend(rule.head.iter().map(|op| (op, None)));
        }
        let generic: Vec<rel::Rule<'_>> = rules
            .iter()
            .map(|rule| rel::Rule {
                name: &rule.name,
                plan: rule.plan.generic(),
                head_vars: rule.head_vars,
                post_saturation: rule.post_saturation,
            })
            .collect();
        let mut caches = std::mem::take(self.caches.get_mut());
        let mut run = Run {
            eg: self,
            rules,
            externs,
            starts,
            ops,
        };
        saturate(
            &mut run,
            &mut caches,
            CLASSES,
            &generic,
            iter_limit,
            node_limit,
        );
        *self.caches.get_mut() = caches;
    }
}
