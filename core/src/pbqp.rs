use std::collections::{BTreeSet, VecDeque};

pub const INF_COST: u64 = u64::MAX / 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PbqpNodeId(u32);

impl PbqpNodeId {
    pub fn from_index(index: usize) -> Self {
        Self(index as u32)
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PbqpAlternative {
    pub node: PbqpNodeId,
    pub alternative: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PbqpMatrix {
    rows: usize,
    cols: usize,
    costs: Vec<u64>,
}

impl PbqpMatrix {
    pub fn new(rows: usize, cols: usize, costs: Vec<u64>) -> Self {
        assert_eq!(rows * cols, costs.len(), "invalid PBQP matrix shape");
        Self { rows, cols, costs }
    }

    pub fn zero(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            costs: vec![0; rows * cols],
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn get(&self, row: usize, col: usize) -> u64 {
        self.costs[row * self.cols + col]
    }

    pub fn set(&mut self, row: usize, col: usize, cost: u64) {
        self.costs[row * self.cols + col] = cost;
    }

    fn add_assign_matrix(&mut self, other: &PbqpMatrix) {
        debug_assert_eq!((self.rows, self.cols), (other.rows, other.cols));
        for (cost, added) in self.costs.iter_mut().zip(&other.costs) {
            *cost = add_cost(*cost, *added);
        }
    }

    fn is_zero(&self) -> bool {
        self.costs.iter().all(|&cost| cost == 0)
    }
}

/// One stored edge, oriented so that `lhs < rhs` and the matrix's rows index
/// `lhs`'s alternatives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PbqpEdge {
    lhs: u32,
    rhs: u32,
    matrix: PbqpMatrix,
}

/// A neighbor together with the edge reaching it, so walking a neighborhood
/// never searches for the connecting cost matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Adjacent {
    neighbor: u32,
    edge: u32,
}

/// Per-node adjacency over a flat edge arena. Adjacency lists stay sorted by
/// neighbor, so every traversal is reproducible regardless of insertion order
/// and a pair lookup is a binary search over one node's degree rather than a
/// search over every edge in the problem. Removed arena slots are recycled, so
/// the reduction/rollback churn of a solve does not grow the arena without
/// bound.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct EdgeStore {
    edges: Vec<Option<PbqpEdge>>,
    free: Vec<u32>,
    adjacency: Vec<Vec<Adjacent>>,
}

impl EdgeStore {
    fn add_node(&mut self) {
        self.adjacency.push(Vec::new());
    }

    fn slots(&self) -> usize {
        self.edges.len()
    }

    fn slot_mut(&mut self, slot: usize) -> Option<&mut PbqpEdge> {
        self.edges[slot].as_mut()
    }

    fn iter(&self) -> impl Iterator<Item = &PbqpEdge> {
        self.edges.iter().flatten()
    }

    fn neighbors(&self, node: usize) -> &[Adjacent] {
        &self.adjacency[node]
    }

    fn degree(&self, node: usize) -> usize {
        self.adjacency[node].len()
    }

    fn edge(&self, edge: u32) -> &PbqpEdge {
        self.edges[edge as usize]
            .as_ref()
            .expect("PBQP edge index must be live")
    }

    fn matrix_mut(&mut self, edge: u32) -> &mut PbqpMatrix {
        &mut self.edges[edge as usize]
            .as_mut()
            .expect("PBQP edge index must be live")
            .matrix
    }

    fn find(&self, lhs: usize, rhs: usize) -> Option<u32> {
        self.position(lhs, rhs)
            .map(|position| self.adjacency[lhs][position].edge)
    }

    fn position(&self, node: usize, neighbor: usize) -> Option<usize> {
        self.adjacency[node]
            .binary_search_by_key(&(neighbor as u32), |adjacent| adjacent.neighbor)
            .ok()
    }

    /// Store an edge already oriented as `lhs < rhs`; the pair must be absent.
    fn insert(&mut self, lhs: usize, rhs: usize, matrix: PbqpMatrix) -> u32 {
        let edge = PbqpEdge {
            lhs: lhs as u32,
            rhs: rhs as u32,
            matrix,
        };
        let index = match self.free.pop() {
            Some(slot) => {
                self.edges[slot as usize] = Some(edge);
                slot
            }
            None => {
                self.edges.push(Some(edge));
                (self.edges.len() - 1) as u32
            }
        };
        self.link(lhs, rhs, index);
        self.link(rhs, lhs, index);
        index
    }

    fn link(&mut self, node: usize, neighbor: usize, edge: u32) {
        let entry = Adjacent {
            neighbor: neighbor as u32,
            edge,
        };
        let position = self.adjacency[node]
            .binary_search_by_key(&entry.neighbor, |adjacent| adjacent.neighbor)
            .expect_err("a PBQP edge must not be inserted twice");
        self.adjacency[node].insert(position, entry);
    }

    fn remove(&mut self, lhs: usize, rhs: usize) -> Option<PbqpMatrix> {
        let edge = self.find(lhs, rhs)?;
        self.unlink(lhs, rhs);
        self.unlink(rhs, lhs);
        self.free.push(edge);
        self.edges[edge as usize]
            .take()
            .map(|removed| removed.matrix)
    }

    fn unlink(&mut self, node: usize, neighbor: usize) {
        let position = self
            .position(node, neighbor)
            .expect("PBQP adjacency must be symmetric");
        self.adjacency[node].remove(position);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PbqpProblem {
    node_costs: Vec<Vec<u64>>,
    edges: EdgeStore,
    coherence_sets: Vec<Vec<PbqpAlternative>>,
}

impl PbqpProblem {
    pub fn new() -> Self {
        Self {
            node_costs: Vec::new(),
            edges: EdgeStore::default(),
            coherence_sets: Vec::new(),
        }
    }

    pub fn add_node(&mut self, costs: Vec<u64>) -> PbqpNodeId {
        assert!(!costs.is_empty(), "PBQP node must have alternatives");
        let id = PbqpNodeId::from_index(self.node_costs.len());
        self.node_costs.push(costs);
        self.edges.add_node();
        id
    }

    pub fn add_edge(&mut self, lhs: PbqpNodeId, rhs: PbqpNodeId, matrix: PbqpMatrix) {
        assert_ne!(lhs, rhs, "PBQP self-edges are not supported");
        let (a, b, matrix) = orient_matrix(lhs, rhs, matrix);
        assert_eq!(self.node_costs[a].len(), matrix.rows());
        assert_eq!(self.node_costs[b].len(), matrix.cols());

        match self.edges.find(a, b) {
            Some(edge) => self.edges.matrix_mut(edge).add_assign_matrix(&matrix),
            None => {
                self.edges.insert(a, b, matrix);
            }
        }
    }

    pub fn add_coherence_set(&mut self, alternatives: Vec<PbqpAlternative>) {
        if alternatives.len() > 1 {
            self.coherence_sets.push(alternatives);
        }
    }

    pub fn node_count(&self) -> usize {
        self.node_costs.len()
    }
}

impl Default for PbqpProblem {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PbqpSolution {
    pub choices: Vec<usize>,
    pub total_cost: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PbqpSolveError {
    Infeasible { node: PbqpNodeId },
    InvalidProblem(String),
}

#[derive(Clone, Debug)]
enum Reduction {
    Fixed {
        node: usize,
        alternative: usize,
    },
    R1 {
        node: usize,
        neighbor: usize,
        choices_by_neighbor_alt: Vec<Option<usize>>,
    },
    R2 {
        node: usize,
        left: usize,
        right: usize,
        right_alternatives: usize,
        choices_by_neighbor_alts: Vec<Option<usize>>,
    },
}

enum Undo {
    NodeCost {
        node: usize,
        alternative: usize,
        old_cost: u64,
    },
    EdgeAdded {
        lhs: usize,
        rhs: usize,
    },
    EdgeRemoved {
        lhs: usize,
        rhs: usize,
        matrix: PbqpMatrix,
    },
    EdgeChanged {
        lhs: usize,
        rhs: usize,
        old_matrix: PbqpMatrix,
    },
    NodeDeactivated {
        node: usize,
    },
}

#[derive(Clone, Copy)]
struct Checkpoint {
    undo_len: usize,
    reductions_len: usize,
}

/// The decision made at a degree ≥ 3 node, where exact reductions no longer
/// apply: which alternatives to force, in which order. The solver tries them in
/// order and backtracks out of any choice that proves globally infeasible, so a
/// heuristic trades search time and solution quality but never correctness —
/// infeasibility is reported only after every finite alternative failed.
pub trait RnHeuristic {
    /// The alternatives of [`RnView::node`] to try, most preferred first.
    /// Choices outside the node's finite alternatives are ignored; an empty
    /// result (or one with no finite choice) fails the node.
    fn order(&mut self, view: &RnView) -> Vec<usize>;
}

/// A read-only view of the solver state at an RN decision point, for
/// [`RnHeuristic`] implementations. Costs are the reduced instance's: node
/// costs folded from exact reductions so far, edge costs between active nodes.
pub struct RnView<'a> {
    solver: &'a Solver,
    node: usize,
}

impl RnView<'_> {
    /// The node being decided.
    pub fn node(&self) -> PbqpNodeId {
        PbqpNodeId::from_index(self.node)
    }

    /// The node's not-yet-impossible alternatives with their current costs.
    pub fn finite_alternatives(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.solver.problem.node_costs[self.node]
            .iter()
            .enumerate()
            .filter(|(_, cost)| **cost < INF_COST)
            .map(|(alternative, &cost)| (alternative, cost))
    }

    /// The active neighbors of the decided node.
    pub fn neighbors(&self) -> Vec<PbqpNodeId> {
        self.solver
            .problem
            .edges
            .neighbors(self.node)
            .iter()
            .map(|adjacent| PbqpNodeId::from_index(adjacent.neighbor as usize))
            .collect()
    }

    /// The number of alternatives `node` has (including impossible ones).
    pub fn alternative_count(&self, node: PbqpNodeId) -> usize {
        self.solver.problem.node_costs[node.index()].len()
    }

    /// The current cost of one alternative of one node.
    pub fn node_cost(&self, node: PbqpNodeId, alternative: usize) -> u64 {
        self.solver.problem.node_costs[node.index()][alternative]
    }

    /// The cost the edge between `lhs` and `rhs` charges for choosing
    /// `lhs_alternative` and `rhs_alternative`.
    pub fn edge_cost(
        &self,
        lhs: PbqpNodeId,
        lhs_alternative: usize,
        rhs: PbqpNodeId,
        rhs_alternative: usize,
    ) -> u64 {
        self.solver
            .edge_cost(lhs.index(), lhs_alternative, rhs.index(), rhs_alternative)
    }
}

/// The default RN heuristic: alternatives ordered by local cost (own cost plus
/// the best achievable neighbor cost over each incident edge).
pub struct LocalCostOrder;

impl RnHeuristic for LocalCostOrder {
    fn order(&mut self, view: &RnView) -> Vec<usize> {
        let neighbors = view.neighbors();
        let mut alternatives: Vec<(usize, u64)> = view
            .finite_alternatives()
            .map(|(alternative, base)| {
                let edge_costs = neighbors.iter().copied().fold(0, |acc, neighbor| {
                    let best = (0..view.alternative_count(neighbor))
                        .filter(|&neighbor_alt| view.node_cost(neighbor, neighbor_alt) < INF_COST)
                        .map(|neighbor_alt| {
                            add_cost(
                                view.node_cost(neighbor, neighbor_alt),
                                view.edge_cost(view.node(), alternative, neighbor, neighbor_alt),
                            )
                        })
                        .min()
                        .unwrap_or(INF_COST);
                    add_cost(acc, best)
                });
                (alternative, add_cost(base, edge_costs))
            })
            .collect();
        alternatives.sort_by_key(|(alternative, cost)| (*cost, *alternative));
        alternatives
            .into_iter()
            .map(|(alternative, _)| alternative)
            .collect()
    }
}

pub fn solve(problem: &PbqpProblem) -> Result<PbqpSolution, PbqpSolveError> {
    solve_with(problem, &mut LocalCostOrder)
}

/// Solve `problem`, delegating RN decisions (degree ≥ 3 nodes) to `heuristic`.
/// This is the extension point for experimenting with alternative orderings —
/// e.g. a learned model scoring alternatives — without touching the exact
/// reductions or the backtracking correctness net.
pub fn solve_with(
    problem: &PbqpProblem,
    heuristic: &mut dyn RnHeuristic,
) -> Result<PbqpSolution, PbqpSolveError> {
    Solver::new(problem.clone()).solve(problem, heuristic)
}

struct Solver {
    problem: PbqpProblem,
    active: Vec<bool>,
    active_count: usize,
    reductions: Vec<Reduction>,
    reducible: BTreeSet<usize>,
    finite_alternatives: Vec<usize>,
    coherence_groups_by_alternative: Vec<Vec<Vec<usize>>>,
    infeasible: BTreeSet<usize>,
    recording_undo: bool,
    undo: Vec<Undo>,
}

impl Solver {
    fn new(problem: PbqpProblem) -> Self {
        let node_count = problem.node_count();
        let active = vec![true; node_count];
        let reducible = (0..node_count)
            .filter(|&node| problem.edges.degree(node) <= 2)
            .collect();
        let finite_alternatives: Vec<usize> = problem
            .node_costs
            .iter()
            .map(|costs| costs.iter().filter(|&&cost| cost < INF_COST).count())
            .collect();
        let infeasible = finite_alternatives
            .iter()
            .enumerate()
            .filter_map(|(node, &finite)| (finite == 0).then_some(node))
            .collect();
        let mut coherence_groups_by_alternative: Vec<Vec<Vec<usize>>> = problem
            .node_costs
            .iter()
            .map(|costs| vec![Vec::new(); costs.len()])
            .collect();
        for (group_index, group) in problem.coherence_sets.iter().enumerate() {
            for alternative in group {
                let node = alternative.node.index();
                if let Some(groups) = coherence_groups_by_alternative
                    .get_mut(node)
                    .and_then(|alternatives| alternatives.get_mut(alternative.alternative))
                {
                    groups.push(group_index);
                }
            }
        }
        Self {
            problem,
            active,
            active_count: node_count,
            reductions: Vec::new(),
            reducible,
            finite_alternatives,
            coherence_groups_by_alternative,
            infeasible,
            recording_undo: false,
            undo: Vec::new(),
        }
    }

    fn solve(
        mut self,
        original: &PbqpProblem,
        heuristic: &mut dyn RnHeuristic,
    ) -> Result<PbqpSolution, PbqpSolveError> {
        self.validate()?;

        // Normalize costs and propagate impossible alternatives once, up front. This
        // is where INF node costs (pre-coloring) and coherence-set fragments get
        // pruned. Re-running the global propagation after every reduction is
        // quadratic in the accumulated INF entries and dominated solve time at
        // register-allocation scale; the per-node reductions below already respect
        // INF through saturating cost arithmetic, so a single pass suffices.
        self.normalize_and_propagate()?;
        self.undo.clear();

        self.solve_prepared(original, heuristic)
    }

    fn solve_prepared(
        &mut self,
        original: &PbqpProblem,
        heuristic: &mut dyn RnHeuristic,
    ) -> Result<PbqpSolution, PbqpSolveError> {
        while self.active_count > 0 {
            let node = self
                .next_active_node()
                .expect("an active PBQP node must be available");
            match self.degree(node) {
                0 => self.reduce_fixed(node)?,
                1 => self.reduce_r1(node)?,
                2 => self.reduce_r2(node)?,
                _ => return self.solve_rn(node, original, heuristic),
            }
        }

        let choices = self.reconstruct()?;
        let total_cost = evaluate_solution(original, &choices)?;
        Ok(PbqpSolution {
            choices,
            total_cost,
        })
    }

    fn validate(&self) -> Result<(), PbqpSolveError> {
        for (node, costs) in self.problem.node_costs.iter().enumerate() {
            if costs.is_empty() {
                return Err(PbqpSolveError::InvalidProblem(format!(
                    "node {node} has no alternatives"
                )));
            }
        }

        for alternatives in &self.problem.coherence_sets {
            for alternative in alternatives {
                let node = alternative.node.index();
                if node >= self.problem.node_costs.len()
                    || alternative.alternative >= self.problem.node_costs[node].len()
                {
                    return Err(PbqpSolveError::InvalidProblem(
                        "coherence set references an unknown alternative".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn normalize_and_propagate(&mut self) -> Result<(), PbqpSolveError> {
        loop {
            let normalized = self.normalize_edges();
            self.rebuild_infeasible();
            let propagated = self.propagate_infinities();
            self.ensure_feasible()?;
            if !normalized && !propagated {
                return Ok(());
            }
        }
    }

    fn normalize_edges(&mut self) -> bool {
        let mut changed = false;
        let mut zero_edges = Vec::new();

        for slot in 0..self.problem.edges.slots() {
            let Solver {
                problem,
                active,
                finite_alternatives,
                ..
            } = self;
            let PbqpProblem {
                node_costs, edges, ..
            } = problem;
            let Some(edge) = edges.slot_mut(slot) else {
                continue;
            };
            let (lhs, rhs) = (edge.lhs as usize, edge.rhs as usize);
            if !active[lhs] || !active[rhs] {
                continue;
            }

            let matrix = &mut edge.matrix;
            changed |= normalize_axis(
                matrix,
                &mut node_costs[lhs],
                &mut finite_alternatives[lhs],
                Axis::Rows,
            );
            changed |= normalize_axis(
                matrix,
                &mut node_costs[rhs],
                &mut finite_alternatives[rhs],
                Axis::Cols,
            );

            if matrix.is_zero() {
                zero_edges.push((lhs, rhs));
            }
        }

        for (lhs, rhs) in zero_edges {
            self.remove_edge(lhs, rhs);
            changed = true;
        }

        changed
    }

    fn propagate_infinities(&mut self) -> bool {
        let mut queue: VecDeque<PbqpAlternative> = self
            .problem
            .node_costs
            .iter()
            .enumerate()
            .flat_map(|(node, costs)| {
                costs
                    .iter()
                    .enumerate()
                    .filter(|(_, cost)| **cost >= INF_COST)
                    .map(move |(alternative, _)| PbqpAlternative {
                        node: PbqpNodeId::from_index(node),
                        alternative,
                    })
            })
            .collect();
        self.propagate_queue(&mut queue)
    }

    fn propagate_queue(&mut self, queue: &mut VecDeque<PbqpAlternative>) -> bool {
        let mut changed = false;

        while let Some(impossible) = queue.pop_front() {
            let node = impossible.node.index();
            let group_indices =
                self.coherence_groups_by_alternative[node][impossible.alternative].clone();
            for group_index in group_indices {
                let coherent_members = self.problem.coherence_sets[group_index].clone();
                for member in coherent_members {
                    if self.mark_impossible(member) {
                        queue.push_back(member);
                        changed = true;
                    }
                }
            }

            for adjacent in self.neighbors(node) {
                let neighbor = adjacent.neighbor as usize;
                for alternative in 0..self.problem.node_costs[neighbor].len() {
                    let candidate = PbqpAlternative {
                        node: PbqpNodeId::from_index(neighbor),
                        alternative,
                    };
                    if self.problem.node_costs[neighbor][alternative] >= INF_COST {
                        continue;
                    }
                    if !self.has_supported_pair(adjacent.edge, candidate, node)
                        && self.mark_impossible(candidate)
                    {
                        queue.push_back(candidate);
                        changed = true;
                    }
                }
            }
        }

        changed
    }

    fn propagate_new_infinities(
        &mut self,
        mut queue: VecDeque<PbqpAlternative>,
    ) -> Result<(), PbqpSolveError> {
        self.propagate_queue(&mut queue);
        self.ensure_feasible()
    }

    fn has_supported_pair(&self, edge: u32, alternative: PbqpAlternative, neighbor: usize) -> bool {
        let node = alternative.node.index();
        (0..self.problem.node_costs[neighbor].len()).any(|neighbor_alt| {
            self.problem.node_costs[neighbor][neighbor_alt] < INF_COST
                && self.edge_cost_at(edge, node, alternative.alternative, neighbor_alt) < INF_COST
        })
    }

    fn mark_impossible(&mut self, alternative: PbqpAlternative) -> bool {
        let node = alternative.node.index();
        let old_cost = self.problem.node_costs[node][alternative.alternative];
        if old_cost >= INF_COST {
            return false;
        }
        if self.recording_undo {
            self.undo.push(Undo::NodeCost {
                node,
                alternative: alternative.alternative,
                old_cost,
            });
        }
        self.problem.node_costs[node][alternative.alternative] = INF_COST;
        self.finite_alternatives[node] -= 1;
        self.refresh_feasibility(node);
        true
    }

    fn add_node_cost(&mut self, node: usize, alternative: usize, cost: u64) -> bool {
        let old_cost = self.problem.node_costs[node][alternative];
        let new_cost = add_cost(old_cost, cost);
        if new_cost == old_cost {
            return false;
        }
        if self.recording_undo {
            self.undo.push(Undo::NodeCost {
                node,
                alternative,
                old_cost,
            });
        }
        self.problem.node_costs[node][alternative] = new_cost;
        if old_cost < INF_COST && new_cost >= INF_COST {
            self.finite_alternatives[node] -= 1;
            self.refresh_feasibility(node);
            true
        } else {
            false
        }
    }

    fn ensure_feasible(&self) -> Result<(), PbqpSolveError> {
        if let Some(&node) = self.infeasible.first() {
            return Err(PbqpSolveError::Infeasible {
                node: PbqpNodeId::from_index(node),
            });
        }
        Ok(())
    }

    fn next_active_node(&self) -> Option<usize> {
        self.reducible
            .first()
            .copied()
            .or_else(|| self.active.iter().position(|active| *active))
    }

    fn degree(&self, node: usize) -> usize {
        self.problem.edges.degree(node)
    }

    /// The node's neighborhood, in ascending neighbor order, copied out so the
    /// caller may mutate the solver while walking it.
    fn neighbors(&self, node: usize) -> Vec<Adjacent> {
        self.problem.edges.neighbors(node).to_vec()
    }

    fn reduce_fixed(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        let alternative = self.cheapest_alternative(node)?;
        self.reductions.push(Reduction::Fixed { node, alternative });
        self.deactivate(node);
        Ok(())
    }

    fn reduce_r1(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        let adjacent = self.problem.edges.neighbors(node)[0];
        let neighbor = adjacent.neighbor as usize;
        let mut choices = vec![None; self.problem.node_costs[neighbor].len()];
        let mut impossible = VecDeque::new();

        for (neighbor_alt, choice) in choices.iter_mut().enumerate() {
            let mut best = INF_COST;
            let mut best_alt = None;
            for node_alt in 0..self.problem.node_costs[node].len() {
                let cost = add_cost(
                    self.problem.node_costs[node][node_alt],
                    self.edge_cost_at(adjacent.edge, node, node_alt, neighbor_alt),
                );
                if cost < best {
                    best = cost;
                    best_alt = Some(node_alt);
                }
            }
            if self.add_node_cost(neighbor, neighbor_alt, best) {
                impossible.push_back(PbqpAlternative {
                    node: PbqpNodeId::from_index(neighbor),
                    alternative: neighbor_alt,
                });
            }
            *choice = best_alt;
        }

        self.remove_incident_edges(node);
        self.deactivate(node);
        self.reductions.push(Reduction::R1 {
            node,
            neighbor,
            choices_by_neighbor_alt: choices,
        });
        self.propagate_new_infinities(impossible)
    }

    fn reduce_r2(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        // Adjacency is kept in ascending neighbor order, so the fill-in edge is
        // already oriented and needs no transpose.
        let neighborhood = self.problem.edges.neighbors(node);
        let (to_left, to_right) = (neighborhood[0], neighborhood[1]);
        let left = to_left.neighbor as usize;
        let right = to_right.neighbor as usize;
        let node_alternatives = self.problem.node_costs[node].len();
        let mut folded = PbqpMatrix::zero(
            self.problem.node_costs[left].len(),
            self.problem.node_costs[right].len(),
        );
        let mut choices = vec![None; folded.rows() * folded.cols()];
        let mut left_side = Vec::with_capacity(node_alternatives);

        for left_alt in 0..folded.rows() {
            left_side.clear();
            left_side.extend((0..node_alternatives).map(|node_alt| {
                add_cost(
                    self.problem.node_costs[node][node_alt],
                    self.edge_cost_at(to_left.edge, left, left_alt, node_alt),
                )
            }));
            for right_alt in 0..folded.cols() {
                let mut best = INF_COST;
                let mut best_alt = None;
                for (node_alt, &left_cost) in left_side.iter().enumerate() {
                    if left_cost >= best {
                        continue;
                    }
                    let cost = add_cost(
                        left_cost,
                        self.edge_cost_at(to_right.edge, node, node_alt, right_alt),
                    );
                    if cost < best {
                        best = cost;
                        best_alt = Some(node_alt);
                    }
                }
                folded.set(left_alt, right_alt, best);
                choices[left_alt * folded.cols() + right_alt] = best_alt;
            }
        }

        self.remove_incident_edges(node);
        self.deactivate(node);
        let fill_in = self.add_or_accumulate_edge(left, right, folded);
        self.reductions.push(Reduction::R2 {
            node,
            left,
            right,
            right_alternatives: self.problem.node_costs[right].len(),
            choices_by_neighbor_alts: choices,
        });
        let mut impossible = VecDeque::new();
        self.prune_unsupported_alternatives(fill_in, left, right, &mut impossible);
        self.propagate_new_infinities(impossible)
    }

    fn solve_rn(
        &mut self,
        node: usize,
        original: &PbqpProblem,
        heuristic: &mut dyn RnHeuristic,
    ) -> Result<PbqpSolution, PbqpSolveError> {
        let alternatives = {
            let view = RnView { solver: self, node };
            heuristic.order(&view)
        };
        // A heuristic may name impossible or out-of-range alternatives; only
        // finite ones are actionable.
        let alternatives: Vec<usize> = alternatives
            .into_iter()
            .filter(|&alternative| {
                alternative < self.problem.node_costs[node].len()
                    && self.problem.node_costs[node][alternative] < INF_COST
            })
            .collect();
        self.recording_undo = true;
        let checkpoint = self.checkpoint();
        for alternative in alternatives {
            self.rollback(checkpoint);
            if let Err(PbqpSolveError::Infeasible { .. }) = self.reduce_rn(node, alternative) {
                continue;
            }
            match self.solve_prepared(original, heuristic) {
                Ok(solution) => return Ok(solution),
                Err(PbqpSolveError::Infeasible { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        self.rollback(checkpoint);
        Err(PbqpSolveError::Infeasible {
            node: PbqpNodeId::from_index(node),
        })
    }

    fn reduce_rn(&mut self, node: usize, alternative: usize) -> Result<(), PbqpSolveError> {
        let mut impossible = VecDeque::new();
        for adjacent in self.neighbors(node) {
            let neighbor = adjacent.neighbor as usize;
            for neighbor_alt in 0..self.problem.node_costs[neighbor].len() {
                let cost = self.edge_cost_at(adjacent.edge, node, alternative, neighbor_alt);
                if self.add_node_cost(neighbor, neighbor_alt, cost) {
                    impossible.push_back(PbqpAlternative {
                        node: PbqpNodeId::from_index(neighbor),
                        alternative: neighbor_alt,
                    });
                }
            }
        }

        self.remove_incident_edges(node);
        self.deactivate(node);
        self.reductions.push(Reduction::Fixed { node, alternative });
        self.propagate_new_infinities(impossible)
    }

    fn cheapest_alternative(&self, node: usize) -> Result<usize, PbqpSolveError> {
        self.problem.node_costs[node]
            .iter()
            .enumerate()
            .filter(|(_, cost)| **cost < INF_COST)
            .min_by_key(|(alternative, cost)| (*cost, *alternative))
            .map(|(alternative, _)| alternative)
            .ok_or(PbqpSolveError::Infeasible {
                node: PbqpNodeId::from_index(node),
            })
    }

    fn edge_cost(&self, lhs: usize, lhs_alt: usize, rhs: usize, rhs_alt: usize) -> u64 {
        self.problem
            .edges
            .find(lhs, rhs)
            .map_or(0, |edge| self.edge_cost_at(edge, lhs, lhs_alt, rhs_alt))
    }

    /// The cost `edge` charges, read from the endpoint `node`'s side. Every
    /// reduction reaches its edges through the adjacency lists, so the matrix is
    /// indexed directly instead of being searched for by node pair.
    #[inline]
    fn edge_cost_at(&self, edge: u32, node: usize, node_alt: usize, other_alt: usize) -> u64 {
        let edge = self.problem.edges.edge(edge);
        if edge.lhs as usize == node {
            edge.matrix.get(node_alt, other_alt)
        } else {
            edge.matrix.get(other_alt, node_alt)
        }
    }

    /// Merge `matrix` into the edge between `lhs` and `rhs`, creating it if the
    /// pair is not yet connected, and answer the edge it landed on. Merging
    /// accumulates into the stored matrix in place, so R2 fill-in costs a single
    /// pass over the fill-in matrix and no reallocation.
    fn add_or_accumulate_edge(&mut self, lhs: usize, rhs: usize, matrix: PbqpMatrix) -> u32 {
        let (a, b, matrix) = orient_matrix(
            PbqpNodeId::from_index(lhs),
            PbqpNodeId::from_index(rhs),
            matrix,
        );
        match self.problem.edges.find(a, b) {
            Some(edge) => {
                if self.recording_undo {
                    self.undo.push(Undo::EdgeChanged {
                        lhs: a,
                        rhs: b,
                        old_matrix: self.problem.edges.edge(edge).matrix.clone(),
                    });
                }
                self.problem
                    .edges
                    .matrix_mut(edge)
                    .add_assign_matrix(&matrix);
                edge
            }
            None => {
                let edge = self.insert_edge_raw(a, b, matrix);
                if self.recording_undo {
                    self.undo.push(Undo::EdgeAdded { lhs: a, rhs: b });
                }
                edge
            }
        }
    }

    fn prune_unsupported_alternatives(
        &mut self,
        edge: u32,
        lhs: usize,
        rhs: usize,
        impossible: &mut VecDeque<PbqpAlternative>,
    ) {
        for (node, neighbor) in [(lhs, rhs), (rhs, lhs)] {
            for alternative in 0..self.problem.node_costs[node].len() {
                let candidate = PbqpAlternative {
                    node: PbqpNodeId::from_index(node),
                    alternative,
                };
                if self.problem.node_costs[node][alternative] < INF_COST
                    && !self.has_supported_pair(edge, candidate, neighbor)
                    && self.mark_impossible(candidate)
                {
                    impossible.push_back(candidate);
                }
            }
        }
    }

    fn remove_incident_edges(&mut self, node: usize) {
        while let Some(adjacent) = self.problem.edges.neighbors(node).first().copied() {
            self.remove_edge(node, adjacent.neighbor as usize);
        }
    }

    fn remove_edge(&mut self, lhs: usize, rhs: usize) {
        let key = if lhs < rhs { (lhs, rhs) } else { (rhs, lhs) };
        if let Some(matrix) = self.remove_edge_raw(key.0, key.1)
            && self.recording_undo
        {
            self.undo.push(Undo::EdgeRemoved {
                lhs: key.0,
                rhs: key.1,
                matrix,
            });
        }
    }

    fn remove_edge_raw(&mut self, lhs: usize, rhs: usize) -> Option<PbqpMatrix> {
        let matrix = self.problem.edges.remove(lhs, rhs)?;
        self.refresh_reducible(lhs);
        self.refresh_reducible(rhs);
        Some(matrix)
    }

    fn insert_edge_raw(&mut self, lhs: usize, rhs: usize, matrix: PbqpMatrix) -> u32 {
        let edge = self.problem.edges.insert(lhs, rhs, matrix);
        self.refresh_reducible(lhs);
        self.refresh_reducible(rhs);
        edge
    }

    fn deactivate(&mut self, node: usize) {
        debug_assert_eq!(self.degree(node), 0);
        if self.recording_undo {
            self.undo.push(Undo::NodeDeactivated { node });
        }
        self.active[node] = false;
        self.active_count -= 1;
        self.reducible.remove(&node);
        self.refresh_feasibility(node);
    }

    fn refresh_reducible(&mut self, node: usize) {
        if self.active[node] && self.problem.edges.degree(node) <= 2 {
            self.reducible.insert(node);
        } else {
            self.reducible.remove(&node);
        }
    }

    fn refresh_feasibility(&mut self, node: usize) {
        if self.active[node] && self.finite_alternatives[node] == 0 {
            self.infeasible.insert(node);
        } else {
            self.infeasible.remove(&node);
        }
    }

    fn rebuild_infeasible(&mut self) {
        self.infeasible.clear();
        for node in 0..self.active.len() {
            if self.active[node] && self.finite_alternatives[node] == 0 {
                self.infeasible.insert(node);
            }
        }
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            undo_len: self.undo.len(),
            reductions_len: self.reductions.len(),
        }
    }

    fn rollback(&mut self, checkpoint: Checkpoint) {
        while self.undo.len() > checkpoint.undo_len {
            match self.undo.pop().unwrap() {
                Undo::NodeCost {
                    node,
                    alternative,
                    old_cost,
                } => {
                    let cost = &mut self.problem.node_costs[node][alternative];
                    if *cost >= INF_COST && old_cost < INF_COST {
                        self.finite_alternatives[node] += 1;
                    } else if *cost < INF_COST && old_cost >= INF_COST {
                        self.finite_alternatives[node] -= 1;
                    }
                    *cost = old_cost;
                    self.refresh_feasibility(node);
                }
                Undo::EdgeAdded { lhs, rhs } => {
                    self.remove_edge_raw(lhs, rhs)
                        .expect("an added PBQP edge must exist during rollback");
                }
                Undo::EdgeRemoved { lhs, rhs, matrix } => {
                    self.insert_edge_raw(lhs, rhs, matrix);
                }
                Undo::EdgeChanged {
                    lhs,
                    rhs,
                    old_matrix,
                } => {
                    let edge = self
                        .problem
                        .edges
                        .find(lhs, rhs)
                        .expect("a changed PBQP edge must exist during rollback");
                    *self.problem.edges.matrix_mut(edge) = old_matrix;
                }
                Undo::NodeDeactivated { node } => {
                    self.active[node] = true;
                    self.active_count += 1;
                    self.refresh_reducible(node);
                    self.refresh_feasibility(node);
                }
            }
        }
        self.reductions.truncate(checkpoint.reductions_len);
    }

    fn reconstruct(&self) -> Result<Vec<usize>, PbqpSolveError> {
        let mut choices = vec![None; self.problem.node_count()];

        for reduction in self.reductions.iter().rev() {
            match reduction {
                Reduction::Fixed { node, alternative } => {
                    choices[*node] = Some(*alternative);
                }
                Reduction::R1 {
                    node,
                    neighbor,
                    choices_by_neighbor_alt,
                } => {
                    let neighbor_alt = choices[*neighbor].ok_or_else(|| {
                        PbqpSolveError::InvalidProblem("missing R1 neighbor choice".to_string())
                    })?;
                    choices[*node] = choices_by_neighbor_alt[neighbor_alt];
                }
                Reduction::R2 {
                    node,
                    left,
                    right,
                    right_alternatives,
                    choices_by_neighbor_alts,
                } => {
                    let left_alt = choices[*left].ok_or_else(|| {
                        PbqpSolveError::InvalidProblem("missing R2 left choice".to_string())
                    })?;
                    let right_alt = choices[*right].ok_or_else(|| {
                        PbqpSolveError::InvalidProblem("missing R2 right choice".to_string())
                    })?;
                    choices[*node] =
                        choices_by_neighbor_alts[left_alt * *right_alternatives + right_alt];
                }
            }
        }

        choices
            .into_iter()
            .enumerate()
            .map(|(node, choice)| {
                choice.ok_or_else(|| {
                    PbqpSolveError::InvalidProblem(format!("missing choice for node {node}"))
                })
            })
            .collect()
    }
}

fn orient_matrix(
    lhs: PbqpNodeId,
    rhs: PbqpNodeId,
    matrix: PbqpMatrix,
) -> (usize, usize, PbqpMatrix) {
    if lhs.index() < rhs.index() {
        (lhs.index(), rhs.index(), matrix)
    } else {
        let mut transposed = PbqpMatrix::zero(matrix.cols(), matrix.rows());
        for row in 0..matrix.rows() {
            for col in 0..matrix.cols() {
                transposed.set(col, row, matrix.get(row, col));
            }
        }
        (rhs.index(), lhs.index(), transposed)
    }
}

/// Which side of an edge's cost matrix a normalization pass folds into its
/// node's alternative costs.
#[derive(Clone, Copy)]
enum Axis {
    Rows,
    Cols,
}

/// Subtract each line's minimum from the matrix and charge it to the owning
/// node's alternative instead — the standard PBQP normalization, which is what
/// makes an all-zero edge removable.
fn normalize_axis(
    matrix: &mut PbqpMatrix,
    costs: &mut [u64],
    finite_alternatives: &mut usize,
    axis: Axis,
) -> bool {
    let (lines, span) = match axis {
        Axis::Rows => (matrix.rows(), matrix.cols()),
        Axis::Cols => (matrix.cols(), matrix.rows()),
    };
    let cell = |line: usize, offset: usize| match axis {
        Axis::Rows => (line, offset),
        Axis::Cols => (offset, line),
    };

    let mut changed = false;
    for (line, cost) in costs.iter_mut().enumerate().take(lines) {
        if *cost >= INF_COST {
            for offset in 0..span {
                let (row, col) = cell(line, offset);
                matrix.set(row, col, INF_COST);
            }
            continue;
        }

        let min = (0..span)
            .map(|offset| {
                let (row, col) = cell(line, offset);
                matrix.get(row, col)
            })
            .min()
            .unwrap_or(INF_COST);
        if min >= INF_COST {
            add_tracked_cost(cost, finite_alternatives, INF_COST);
            changed = true;
        } else if min > 0 {
            add_tracked_cost(cost, finite_alternatives, min);
            for offset in 0..span {
                let (row, col) = cell(line, offset);
                let current = matrix.get(row, col);
                if current < INF_COST {
                    matrix.set(row, col, current - min);
                }
            }
            changed = true;
        }
    }
    changed
}

fn add_cost(lhs: u64, rhs: u64) -> u64 {
    if lhs >= INF_COST || rhs >= INF_COST {
        INF_COST
    } else {
        lhs.saturating_add(rhs).min(INF_COST)
    }
}

fn add_tracked_cost(cost: &mut u64, finite_alternatives: &mut usize, added: u64) -> bool {
    let old_cost = *cost;
    let new_cost = add_cost(old_cost, added);
    *cost = new_cost;
    if old_cost < INF_COST && new_cost >= INF_COST {
        *finite_alternatives -= 1;
        true
    } else {
        false
    }
}

fn evaluate_solution(problem: &PbqpProblem, choices: &[usize]) -> Result<u64, PbqpSolveError> {
    let mut total = 0;
    for (node, &choice) in choices.iter().enumerate() {
        let Some(cost) = problem.node_costs[node].get(choice) else {
            return Err(PbqpSolveError::InvalidProblem(format!(
                "choice for node {node} is out of range"
            )));
        };
        total = add_cost(total, *cost);
    }

    for edge in problem.edges.iter() {
        total = add_cost(
            total,
            edge.matrix
                .get(choices[edge.lhs as usize], choices[edge.rhs as usize]),
        );
    }

    if total >= INF_COST {
        Err(PbqpSolveError::Infeasible {
            node: PbqpNodeId::from_index(0),
        })
    } else {
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INF_COST, PbqpAlternative, PbqpMatrix, PbqpNodeId, PbqpProblem, RnHeuristic, solve,
        solve_with,
    };

    #[test]
    fn r1_selects_cheapest_compatible_alternatives() {
        let mut problem = PbqpProblem::new();
        let a = problem.add_node(vec![2, 0]);
        let b = problem.add_node(vec![0, 0]);
        problem.add_edge(a, b, PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 3]));

        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices, vec![0, 0]);
        assert_eq!(solution.total_cost, 2);
    }

    #[test]
    fn r2_folds_chain_costs_into_neighbor_matrix() {
        let mut problem = PbqpProblem::new();
        let a = problem.add_node(vec![0, 2]);
        let b = problem.add_node(vec![1, 0]);
        let c = problem.add_node(vec![0, 0]);
        problem.add_edge(a, b, PbqpMatrix::new(2, 2, vec![0, 4, 3, 0]));
        problem.add_edge(b, c, PbqpMatrix::new(2, 2, vec![0, 5, 7, 0]));

        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices, vec![0, 0, 0]);
        assert_eq!(solution.total_cost, 1);
    }

    #[test]
    fn rn_accounts_for_neighbor_instruction_costs() {
        let mut problem = PbqpProblem::new();
        let root = problem.add_node(vec![1, 2]);
        for _ in 0..3 {
            let operand = problem.add_node(vec![0, 10]);
            problem.add_edge(
                root,
                operand,
                PbqpMatrix::new(2, 2, vec![INF_COST, 0, 0, INF_COST]),
            );
        }

        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices[0], 1);
        assert_eq!(solution.total_cost, 2);
    }

    #[test]
    fn coherence_set_propagates_impossible_pattern_fragments() {
        let mut problem = PbqpProblem::new();
        let root = problem.add_node(vec![INF_COST, 0]);
        let child = problem.add_node(vec![5, 0]);
        problem.add_coherence_set(vec![
            PbqpAlternative {
                node: root,
                alternative: 0,
            },
            PbqpAlternative {
                node: child,
                alternative: 1,
            },
        ]);

        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices, vec![1, 0]);
        assert_eq!(solution.total_cost, 5);
    }

    #[test]
    fn rn_keeps_high_degree_instances_solvable() {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(vec![4, 1]);
        let a = problem.add_node(vec![0, 0]);
        let b = problem.add_node(vec![0, 0]);
        let c = problem.add_node(vec![0, 0]);
        let prefer_alt_one = PbqpMatrix::new(2, 2, vec![2, 2, 0, 0]);
        problem.add_edge(center, a, prefer_alt_one.clone());
        problem.add_edge(center, b, prefer_alt_one.clone());
        problem.add_edge(center, c, prefer_alt_one);

        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices[center.index()], 1);
        assert_eq!(solution.total_cost, 1);
    }

    #[test]
    fn exact_reductions_precede_rn_decisions() {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(vec![0, 1]);
        let same_choice = PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 0]);

        for _ in 0..3 {
            let middle = problem.add_node(vec![0, 0]);
            let leaf = problem.add_node(vec![10, 0]);
            problem.add_edge(center, middle, same_choice.clone());
            problem.add_edge(middle, leaf, same_choice.clone());
        }

        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices[center.index()], 1);
        assert_eq!(solution.total_cost, 1);
    }

    #[test]
    fn rn_backtracks_when_local_choice_is_globally_infeasible() {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(vec![0, 1]);
        let left = problem.add_node(vec![0, 0]);
        let right = problem.add_node(vec![0, 0]);
        let third = problem.add_node(vec![0, 0]);
        let same_choice = PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 0]);
        problem.add_edge(center, left, same_choice.clone());
        problem.add_edge(center, right, same_choice.clone());
        problem.add_edge(center, third, same_choice);
        problem.add_edge(left, right, PbqpMatrix::new(2, 2, vec![INF_COST, 0, 0, 0]));

        let solution = solve(&problem).expect("PBQP should try the feasible Rn alternative");
        assert_eq!(solution.choices[center.index()], 1);
        assert_eq!(solution.total_cost, 1);
    }

    #[test]
    fn rn_propagates_branch_infinities_through_coherence_sets() {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(vec![0, 1]);
        let a = problem.add_node(vec![0, 0]);
        let b = problem.add_node(vec![0, INF_COST]);
        let c = problem.add_node(vec![0, 0]);
        let same_choice = PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 0]);
        let penalty = PbqpMatrix::new(2, 2, vec![0, 0, 0, 1]);

        problem.add_edge(center, a, same_choice);
        for (lhs, rhs) in [(center, b), (center, c), (a, b), (a, c), (b, c)] {
            problem.add_edge(lhs, rhs, penalty.clone());
        }
        problem.add_coherence_set(vec![
            PbqpAlternative {
                node: a,
                alternative: 1,
            },
            PbqpAlternative {
                node: b,
                alternative: 0,
            },
        ]);

        let solution = solve(&problem).expect("PBQP should try the coherent Rn alternative");
        assert_eq!(solution.choices[center.index()], 1);
    }

    #[test]
    fn rn_restores_reductions_before_trying_the_next_alternative() {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(vec![0, 1]);
        let a = problem.add_node(vec![0, 0]);
        let b = problem.add_node(vec![0, 0]);
        let c = problem.add_node(vec![0, 0]);
        let same_choice = PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 0]);
        let penalty = PbqpMatrix::new(2, 2, vec![0, 0, 0, 1]);

        problem.add_edge(center, a, same_choice.clone());
        problem.add_edge(center, b, penalty.clone());
        problem.add_edge(center, c, penalty);
        problem.add_edge(a, b, same_choice.clone());
        problem.add_edge(b, c, same_choice);
        problem.add_edge(a, c, PbqpMatrix::new(2, 2, vec![INF_COST, 0, 0, 0]));

        let solution = solve(&problem).expect("PBQP should restore the failed Rn branch");
        assert_eq!(solution.choices[center.index()], 1);
        assert_eq!(solution.total_cost, 3);
    }

    /// Equal-cost optima must not be decided by hash iteration order: the same
    /// problem built with its edges added in a different order must solve the
    /// same way, or the compiler's output depends on the process's hash seed.
    #[test]
    fn solution_is_independent_of_edge_insertion_order() {
        let ring = |reversed: bool| {
            let mut problem = PbqpProblem::new();
            let nodes: Vec<_> = (0..8).map(|_| problem.add_node(vec![0, 0])).collect();
            let differ = PbqpMatrix::new(2, 2, vec![1, 0, 0, 1]);
            let mut edges: Vec<_> = (0..nodes.len())
                .map(|i| (nodes[i], nodes[(i + 1) % nodes.len()]))
                .collect();
            if reversed {
                edges.reverse();
            }
            for (lhs, rhs) in edges {
                problem.add_edge(lhs, rhs, differ.clone());
            }
            solve(&problem).expect("PBQP should be solvable")
        };

        assert_eq!(ring(false), ring(true));
    }

    /// Two edges over the same pair are one edge charging both costs, and the
    /// second one's orientation is the caller's, not the store's.
    #[test]
    fn repeated_edges_accumulate_in_the_stored_orientation() {
        let mut problem = PbqpProblem::new();
        let a = problem.add_node(vec![0, 0]);
        let b = problem.add_node(vec![0, 0]);
        problem.add_edge(a, b, PbqpMatrix::new(2, 2, vec![7, 0, 0, 0]));
        problem.add_edge(b, a, PbqpMatrix::new(2, 2, vec![0, 9, 1, 9]));

        // Charged: [[7, 1], [9, 9]] — cheapest at a=0, b=1.
        let solution = solve(&problem).expect("PBQP should be solvable");
        assert_eq!(solution.choices, vec![0, 1]);
        assert_eq!(solution.total_cost, 1);
    }

    struct ReverseOrder;
    impl RnHeuristic for ReverseOrder {
        fn order(&mut self, view: &super::RnView) -> Vec<usize> {
            let mut alternatives: Vec<usize> =
                view.finite_alternatives().map(|(alt, _)| alt).collect();
            alternatives.reverse();
            alternatives
        }
    }

    #[test]
    fn custom_heuristic_supplies_the_rn_order() {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(vec![0, 1]);
        let left = problem.add_node(vec![0, 0]);
        let right = problem.add_node(vec![0, 0]);
        let third = problem.add_node(vec![0, 0]);
        let same_choice = PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 0]);
        problem.add_edge(center, left, same_choice.clone());
        problem.add_edge(center, right, same_choice.clone());
        problem.add_edge(center, third, same_choice);
        problem.add_edge(left, right, PbqpMatrix::new(2, 2, vec![INF_COST, 0, 0, 0]));

        // Trying the expensive alternative first still solves: the solver
        // backtracks out of it and follows the heuristic's next choice.
        let solution = solve_with(&problem, &mut ReverseOrder).expect("PBQP should be solvable");
        assert_eq!(solution.choices[center.index()], 1);
        assert_eq!(solution.total_cost, 1);
    }

    /// A 4-clique of two-alternative nodes: every node has degree 3, so no
    /// exact reduction applies and the solver must make an RN decision on
    /// `center`.
    fn rn_clique(center_costs: Vec<u64>) -> (PbqpProblem, PbqpNodeId) {
        let mut problem = PbqpProblem::new();
        let center = problem.add_node(center_costs);
        let same_choice = PbqpMatrix::new(2, 2, vec![0, INF_COST, INF_COST, 0]);
        let mut others = Vec::new();
        for _ in 0..3 {
            let node = problem.add_node(vec![0, 0]);
            problem.add_edge(center, node, same_choice.clone());
            for &earlier in &others {
                problem.add_edge(earlier, node, same_choice.clone());
            }
            others.push(node);
        }
        (problem, center)
    }

    struct FirstAlternative;
    impl RnHeuristic for FirstAlternative {
        fn order(&mut self, _: &super::RnView) -> Vec<usize> {
            vec![0]
        }
    }

    #[test]
    fn heuristic_choice_controls_the_solution() {
        // Two feasible RN alternatives at different costs; a heuristic that
        // prefers the expensive one gets it — the ordering is the decision.
        let (problem, center) = rn_clique(vec![5, 1]);

        let solution =
            solve_with(&problem, &mut FirstAlternative).expect("PBQP should be solvable");
        assert_eq!(solution.choices[center.index()], 0);
        assert_eq!(solution.total_cost, 5);
    }

    struct EmptyOrder;
    impl RnHeuristic for EmptyOrder {
        fn order(&mut self, _: &super::RnView) -> Vec<usize> {
            Vec::new()
        }
    }

    #[test]
    fn heuristic_returning_no_alternative_is_infeasible() {
        let (problem, _) = rn_clique(vec![0, 1]);

        assert!(solve_with(&problem, &mut EmptyOrder).is_err());
    }
}
