use std::collections::{HashMap, VecDeque};

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

#[derive(Clone, Debug, PartialEq, Eq)]
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

    fn add_assign(&mut self, row: usize, col: usize, cost: u64) {
        let idx = row * self.cols + col;
        self.costs[idx] = add_cost(self.costs[idx], cost);
    }

    fn is_zero(&self) -> bool {
        self.costs.iter().all(|&cost| cost == 0)
    }
}

#[derive(Clone, Debug)]
pub struct PbqpProblem {
    node_costs: Vec<Vec<u64>>,
    edges: HashMap<(usize, usize), PbqpMatrix>,
    coherence_sets: Vec<Vec<PbqpAlternative>>,
}

impl PbqpProblem {
    pub fn new() -> Self {
        Self {
            node_costs: Vec::new(),
            edges: HashMap::new(),
            coherence_sets: Vec::new(),
        }
    }

    pub fn add_node(&mut self, costs: Vec<u64>) -> PbqpNodeId {
        assert!(!costs.is_empty(), "PBQP node must have alternatives");
        let id = PbqpNodeId::from_index(self.node_costs.len());
        self.node_costs.push(costs);
        id
    }

    pub fn add_edge(&mut self, lhs: PbqpNodeId, rhs: PbqpNodeId, matrix: PbqpMatrix) {
        assert_ne!(lhs, rhs, "PBQP self-edges are not supported");
        let (a, b, matrix) = orient_matrix(lhs, rhs, matrix);
        assert_eq!(self.node_costs[a].len(), matrix.rows());
        assert_eq!(self.node_costs[b].len(), matrix.cols());

        self.edges
            .entry((a, b))
            .and_modify(|existing| {
                for row in 0..existing.rows() {
                    for col in 0..existing.cols() {
                        existing.add_assign(row, col, matrix.get(row, col));
                    }
                }
            })
            .or_insert(matrix);
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

pub fn solve(problem: &PbqpProblem) -> Result<PbqpSolution, PbqpSolveError> {
    Solver::new(problem.clone()).solve(problem)
}

struct Solver {
    problem: PbqpProblem,
    active: Vec<bool>,
    reductions: Vec<Reduction>,
}

impl Solver {
    fn new(problem: PbqpProblem) -> Self {
        let active = vec![true; problem.node_count()];
        Self {
            problem,
            active,
            reductions: Vec::new(),
        }
    }

    fn solve(mut self, original: &PbqpProblem) -> Result<PbqpSolution, PbqpSolveError> {
        self.validate()?;
        self.normalize_and_propagate()?;

        while let Some(node) = self.next_active_node() {
            match self.degree(node) {
                0 => self.reduce_fixed(node)?,
                1 => self.reduce_r1(node)?,
                2 => self.reduce_r2(node)?,
                _ => self.reduce_rn(node)?,
            }
            self.normalize_and_propagate()?;
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
            let propagated = self.propagate_infinities();
            self.ensure_feasible()?;
            if !normalized && !propagated {
                return Ok(());
            }
        }
    }

    fn normalize_edges(&mut self) -> bool {
        let mut changed = false;
        let keys: Vec<_> = self.problem.edges.keys().copied().collect();
        let mut zero_edges = Vec::new();

        for key @ (lhs, rhs) in keys {
            if !self.active[lhs] || !self.active[rhs] {
                continue;
            }

            let matrix = self.problem.edges.get_mut(&key).unwrap();
            for row in 0..matrix.rows() {
                if self.problem.node_costs[lhs][row] >= INF_COST {
                    for col in 0..matrix.cols() {
                        matrix.set(row, col, INF_COST);
                    }
                    continue;
                }

                let min = (0..matrix.cols())
                    .map(|col| matrix.get(row, col))
                    .min()
                    .unwrap_or(INF_COST);
                if min >= INF_COST {
                    self.problem.node_costs[lhs][row] = INF_COST;
                    changed = true;
                } else if min > 0 {
                    self.problem.node_costs[lhs][row] =
                        add_cost(self.problem.node_costs[lhs][row], min);
                    for col in 0..matrix.cols() {
                        let cost = matrix.get(row, col);
                        if cost < INF_COST {
                            matrix.set(row, col, cost - min);
                        }
                    }
                    changed = true;
                }
            }

            for col in 0..matrix.cols() {
                if self.problem.node_costs[rhs][col] >= INF_COST {
                    for row in 0..matrix.rows() {
                        matrix.set(row, col, INF_COST);
                    }
                    continue;
                }

                let min = (0..matrix.rows())
                    .map(|row| matrix.get(row, col))
                    .min()
                    .unwrap_or(INF_COST);
                if min >= INF_COST {
                    self.problem.node_costs[rhs][col] = INF_COST;
                    changed = true;
                } else if min > 0 {
                    self.problem.node_costs[rhs][col] =
                        add_cost(self.problem.node_costs[rhs][col], min);
                    for row in 0..matrix.rows() {
                        let cost = matrix.get(row, col);
                        if cost < INF_COST {
                            matrix.set(row, col, cost - min);
                        }
                    }
                    changed = true;
                }
            }

            if matrix.is_zero() {
                zero_edges.push(key);
            }
        }

        for key in zero_edges {
            self.problem.edges.remove(&key);
            changed = true;
        }

        changed
    }

    fn propagate_infinities(&mut self) -> bool {
        let mut changed = false;
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

        while let Some(impossible) = queue.pop_front() {
            let coherent_members: Vec<_> = self
                .problem
                .coherence_sets
                .iter()
                .filter(|group| group.contains(&impossible))
                .flat_map(|group| group.iter().copied())
                .collect();
            for member in coherent_members {
                if self.mark_impossible(member) {
                    queue.push_back(member);
                    changed = true;
                }
            }

            for neighbor in self.neighbors(impossible.node.index()) {
                for alternative in 0..self.problem.node_costs[neighbor].len() {
                    let candidate = PbqpAlternative {
                        node: PbqpNodeId::from_index(neighbor),
                        alternative,
                    };
                    if self.problem.node_costs[neighbor][alternative] >= INF_COST {
                        continue;
                    }
                    if !self.has_supported_pair(candidate) && self.mark_impossible(candidate) {
                        queue.push_back(candidate);
                        changed = true;
                    }
                }
            }
        }

        changed
    }

    fn has_supported_pair(&self, alternative: PbqpAlternative) -> bool {
        let node = alternative.node.index();
        self.neighbors(node).into_iter().all(|neighbor| {
            (0..self.problem.node_costs[neighbor].len()).any(|neighbor_alt| {
                self.problem.node_costs[neighbor][neighbor_alt] < INF_COST
                    && self.edge_cost(node, alternative.alternative, neighbor, neighbor_alt)
                        < INF_COST
            })
        })
    }

    fn mark_impossible(&mut self, alternative: PbqpAlternative) -> bool {
        let node = alternative.node.index();
        if self.problem.node_costs[node][alternative.alternative] >= INF_COST {
            return false;
        }
        self.problem.node_costs[node][alternative.alternative] = INF_COST;
        true
    }

    fn ensure_feasible(&self) -> Result<(), PbqpSolveError> {
        for (node, costs) in self.problem.node_costs.iter().enumerate() {
            if self.active[node] && costs.iter().all(|&cost| cost >= INF_COST) {
                return Err(PbqpSolveError::Infeasible {
                    node: PbqpNodeId::from_index(node),
                });
            }
        }
        Ok(())
    }

    fn next_active_node(&self) -> Option<usize> {
        self.active.iter().position(|active| *active)
    }

    fn degree(&self, node: usize) -> usize {
        self.neighbors(node).len()
    }

    fn neighbors(&self, node: usize) -> Vec<usize> {
        self.problem
            .edges
            .keys()
            .filter_map(|&(lhs, rhs)| {
                if !self.active[lhs] || !self.active[rhs] {
                    None
                } else if lhs == node {
                    Some(rhs)
                } else if rhs == node {
                    Some(lhs)
                } else {
                    None
                }
            })
            .collect()
    }

    fn reduce_fixed(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        let alternative = self.cheapest_alternative(node)?;
        self.reductions.push(Reduction::Fixed { node, alternative });
        self.active[node] = false;
        Ok(())
    }

    fn reduce_r1(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        let neighbor = self.neighbors(node)[0];
        let mut choices = vec![None; self.problem.node_costs[neighbor].len()];

        for (neighbor_alt, choice) in choices.iter_mut().enumerate() {
            let mut best = INF_COST;
            let mut best_alt = None;
            for node_alt in 0..self.problem.node_costs[node].len() {
                let cost = add_cost(
                    self.problem.node_costs[node][node_alt],
                    self.edge_cost(node, node_alt, neighbor, neighbor_alt),
                );
                if cost < best {
                    best = cost;
                    best_alt = Some(node_alt);
                }
            }
            self.problem.node_costs[neighbor][neighbor_alt] =
                add_cost(self.problem.node_costs[neighbor][neighbor_alt], best);
            *choice = best_alt;
        }

        self.remove_incident_edges(node);
        self.active[node] = false;
        self.reductions.push(Reduction::R1 {
            node,
            neighbor,
            choices_by_neighbor_alt: choices,
        });
        Ok(())
    }

    fn reduce_r2(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        let mut neighbors = self.neighbors(node);
        neighbors.sort_unstable();
        let left = neighbors[0];
        let right = neighbors[1];
        let mut folded = PbqpMatrix::zero(
            self.problem.node_costs[left].len(),
            self.problem.node_costs[right].len(),
        );
        let mut choices = vec![None; folded.rows() * folded.cols()];

        for left_alt in 0..folded.rows() {
            for right_alt in 0..folded.cols() {
                let mut best = INF_COST;
                let mut best_alt = None;
                for node_alt in 0..self.problem.node_costs[node].len() {
                    let left_cost = self.edge_cost(left, left_alt, node, node_alt);
                    let right_cost = self.edge_cost(node, node_alt, right, right_alt);
                    let cost = add_cost(
                        self.problem.node_costs[node][node_alt],
                        add_cost(left_cost, right_cost),
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
        self.active[node] = false;
        self.add_or_accumulate_edge(left, right, folded);
        self.reductions.push(Reduction::R2 {
            node,
            left,
            right,
            right_alternatives: self.problem.node_costs[right].len(),
            choices_by_neighbor_alts: choices,
        });
        Ok(())
    }

    fn reduce_rn(&mut self, node: usize) -> Result<(), PbqpSolveError> {
        let alternative = self.locally_cheapest_alternative(node)?;
        for neighbor in self.neighbors(node) {
            for neighbor_alt in 0..self.problem.node_costs[neighbor].len() {
                let cost = self.edge_cost(node, alternative, neighbor, neighbor_alt);
                self.problem.node_costs[neighbor][neighbor_alt] =
                    add_cost(self.problem.node_costs[neighbor][neighbor_alt], cost);
            }
        }

        self.remove_incident_edges(node);
        self.active[node] = false;
        self.reductions.push(Reduction::Fixed { node, alternative });
        Ok(())
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

    fn locally_cheapest_alternative(&self, node: usize) -> Result<usize, PbqpSolveError> {
        self.problem.node_costs[node]
            .iter()
            .enumerate()
            .filter(|(_, cost)| **cost < INF_COST)
            .map(|(alternative, &base)| {
                let edge_costs = self.neighbors(node).into_iter().fold(0, |acc, neighbor| {
                    let best = (0..self.problem.node_costs[neighbor].len())
                        .filter(|&neighbor_alt| {
                            self.problem.node_costs[neighbor][neighbor_alt] < INF_COST
                        })
                        .map(|neighbor_alt| {
                            self.edge_cost(node, alternative, neighbor, neighbor_alt)
                        })
                        .min()
                        .unwrap_or(INF_COST);
                    add_cost(acc, best)
                });
                (alternative, add_cost(base, edge_costs))
            })
            .min_by_key(|(alternative, cost)| (*cost, *alternative))
            .map(|(alternative, _)| alternative)
            .ok_or(PbqpSolveError::Infeasible {
                node: PbqpNodeId::from_index(node),
            })
    }

    fn edge_cost(&self, lhs: usize, lhs_alt: usize, rhs: usize, rhs_alt: usize) -> u64 {
        let (a, a_alt, b, b_alt) = if lhs < rhs {
            (lhs, lhs_alt, rhs, rhs_alt)
        } else {
            (rhs, rhs_alt, lhs, lhs_alt)
        };
        self.problem
            .edges
            .get(&(a, b))
            .map(|matrix| matrix.get(a_alt, b_alt))
            .unwrap_or(0)
    }

    fn add_or_accumulate_edge(&mut self, lhs: usize, rhs: usize, matrix: PbqpMatrix) {
        let (a, b, matrix) = orient_matrix(
            PbqpNodeId::from_index(lhs),
            PbqpNodeId::from_index(rhs),
            matrix,
        );
        self.problem
            .edges
            .entry((a, b))
            .and_modify(|existing| {
                for row in 0..existing.rows() {
                    for col in 0..existing.cols() {
                        existing.add_assign(row, col, matrix.get(row, col));
                    }
                }
            })
            .or_insert(matrix);
    }

    fn remove_incident_edges(&mut self, node: usize) {
        self.problem
            .edges
            .retain(|&(lhs, rhs), _| lhs != node && rhs != node);
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

fn add_cost(lhs: u64, rhs: u64) -> u64 {
    if lhs >= INF_COST || rhs >= INF_COST {
        INF_COST
    } else {
        lhs.saturating_add(rhs).min(INF_COST)
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

    for (&(lhs, rhs), matrix) in &problem.edges {
        total = add_cost(total, matrix.get(choices[lhs], choices[rhs]));
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
    use super::{INF_COST, PbqpAlternative, PbqpMatrix, PbqpProblem, solve};

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
}
