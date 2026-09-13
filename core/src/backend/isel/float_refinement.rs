use std::collections::{HashMap, HashSet};

use tir::graph::{Dag, MutDag, NodeId, subgraphs_equal};
use tir::sem::{
    FloatFormat, SemGraph, SemType, SmtOracle, StateAccessKind, StateFieldKind, StateResourceKind,
    SymKind, SymPayload, Width, infer_types,
};
use tir_adt::APInt;

#[derive(Clone)]
struct StateView {
    initial: u32,
    fields: HashMap<(u64, u64), NodeId>,
}

struct StateProof<'a> {
    source: &'a SemGraph,
    values: SemGraph,
    types: Vec<SemType>,
    initial_fields: HashMap<(u32, u64, u64), NodeId>,
    state_memo: HashMap<NodeId, StateView>,
    value_memo: HashMap<NodeId, NodeId>,
}

impl StateProof<'_> {
    fn field(&mut self, initial: u32, resource: u64, field: u64, width: u32) -> NodeId {
        *self
            .initial_fields
            .entry((initial, resource, field))
            .or_insert_with(|| {
                let node = self.values.add_node(SymKind::Symbol);
                let symbol = self.types.len() as u32;
                self.values
                    .set_leaf_data(node, SymPayload::SymbolId(symbol));
                self.types.push(SemType::bits(width));
                node
            })
    }

    fn resource_field(&self, resource: NodeId, field: NodeId) -> Option<(u64, u64, u32)> {
        let resource = constant_value(self.source, resource)?;
        let field = constant_value(self.source, field)?;
        let resource_kind = StateResourceKind::from_code(resource)?;
        let field_kind = StateFieldKind::from_code(field)?;
        if field_kind == StateFieldKind::Whole {
            return None;
        }
        let width = resource_kind.field_schema(field_kind)?.bit_width?;
        Some((resource, field, width))
    }

    fn value(&mut self, node: NodeId) -> Option<NodeId> {
        if let Some(&copied) = self.value_memo.get(&node) {
            return Some(copied);
        }
        if *self.source.get_kind(node) == SymKind::StateRead {
            let children: Vec<_> = self.source.children(node).collect();
            let [state, resource, field] = children.as_slice() else {
                return None;
            };
            let (resource, field, width) = self.resource_field(*resource, *field)?;
            let state = self.state(*state)?;
            let value = state
                .fields
                .get(&(resource, field))
                .copied()
                .unwrap_or_else(|| self.field(state.initial, resource, field, width));
            self.value_memo.insert(node, value);
            return Some(value);
        }
        let kind = *self.source.get_kind(node);
        if !super::node::kind_is_pure(kind)
            || matches!(
                kind,
                SymKind::Div
                    | SymKind::UDiv
                    | SymKind::SRem
                    | SymKind::URem
                    | SymKind::FPToSI
                    | SymKind::FPToUI
                    | SymKind::Loop
                    | SymKind::Theta
                    | SymKind::Port
            )
        {
            return None;
        }
        let children: Vec<_> = self
            .source
            .children(node)
            .map(|child| self.value(child))
            .collect::<Option<_>>()?;
        let copied = self.values.add_node(*self.source.get_kind(node));
        if let Some(payload) = self.source.get_leaf_data(node) {
            self.values.set_leaf_data(copied, payload.clone());
        }
        for child in children {
            self.values.add_edge(copied, child);
        }
        self.value_memo.insert(node, copied);
        Some(copied)
    }

    fn equivalent(&self, left: NodeId, right: NodeId) -> bool {
        let mut left_graph = SemGraph::new();
        tir_symbolic::sem::copy_subgraph(&mut left_graph, &self.values, left, &mut HashMap::new());
        let mut right_graph = SemGraph::new();
        tir_symbolic::sem::copy_subgraph(
            &mut right_graph,
            &self.values,
            right,
            &mut HashMap::new(),
        );
        SmtOracle.equivalent_typed(&left_graph, &right_graph, &self.types)
    }

    fn state(&mut self, node: NodeId) -> Option<StateView> {
        if let Some(state) = self.state_memo.get(&node) {
            return Some(state.clone());
        }
        let state = match self.source.get_kind(node) {
            SymKind::Symbol => StateView {
                initial: match self.source.get_leaf_data(node)? {
                    SymPayload::SymbolId(symbol) => *symbol,
                    _ => return None,
                },
                fields: HashMap::new(),
            },
            SymKind::StateAssign => {
                let children: Vec<_> = self.source.children(node).collect();
                let [prior, resource, field, access, value] = children.as_slice() else {
                    return None;
                };
                let (resource, field, width) = self.resource_field(*resource, *field)?;
                let access = match constant_value(self.source, *access)? {
                    value if value == StateAccessKind::Read as u64 => StateAccessKind::Read,
                    value if value == StateAccessKind::Change as u64 => StateAccessKind::Change,
                    _ => return None,
                };
                let mut prior = self.state(*prior)?;
                let value = self.value(*value)?;
                let old = prior
                    .fields
                    .get(&(resource, field))
                    .copied()
                    .unwrap_or_else(|| self.field(prior.initial, resource, field, width));
                match access {
                    StateAccessKind::Read if !self.equivalent(value, old) => return None,
                    StateAccessKind::Read => {}
                    StateAccessKind::Change => {
                        prior.fields.insert((resource, field), value);
                    }
                }
                prior
            }
            _ => return None,
        };
        self.state_memo.insert(node, state.clone());
        Some(state)
    }
}

pub(super) fn states_preserved(graph: &SemGraph, result: NodeId, symbol_types: &[SemType]) -> bool {
    let states: Vec<_> = graph.children(result).skip(1).collect();
    if states.is_empty() {
        return false;
    }
    let mut proof = StateProof {
        source: graph,
        values: SemGraph::new(),
        types: symbol_types.to_vec(),
        initial_fields: HashMap::new(),
        state_memo: HashMap::new(),
        value_memo: HashMap::new(),
    };
    states.into_iter().all(|state| {
        let Some(view) = proof.state(state) else {
            return false;
        };
        view.fields.into_iter().all(|((resource, field), value)| {
            let Some(width) = StateResourceKind::from_code(resource)
                .zip(StateFieldKind::from_code(field))
                .and_then(|(resource, field)| resource.field_schema(field))
                .and_then(|schema| schema.bit_width)
            else {
                return false;
            };
            let initial = proof.field(view.initial, resource, field, width);
            proof.equivalent(value, initial)
        })
    })
}

fn ieee_arithmetic_refines(
    full: &SemGraph,
    candidate: &SemGraph,
    symbol_types: &[Option<SemType>],
) -> bool {
    let (Some(full_root), Some(candidate_root)) = (full.root(), candidate.root()) else {
        return false;
    };
    let full_value = value_result(full, full_root);
    let candidate_value = value_result(candidate, candidate_root);
    let Ok(types) = infer_types(candidate, |node| match candidate.get_leaf_data(node) {
        Some(SymPayload::SymbolId(id)) => symbol_types.get(*id as usize).cloned().flatten(),
        _ => None,
    }) else {
        return false;
    };
    let SemType::Float(format) = &types[candidate_value.index()] else {
        return false;
    };
    let Some((width, nan_mask)) = quiet_nan_mask(format) else {
        return false;
    };
    let mut shared: HashSet<_> = full
        .postorder(full_value)
        .filter(|&node| subgraphs_equal(candidate, candidate_value, full, node))
        .collect();
    if shared.is_empty()
        && let Some(rounded) = rounded_kind(*candidate.get_kind(candidate_value))
    {
        let operands: Vec<_> = candidate.children(candidate_value).collect();
        shared.extend(full.postorder(full_value).filter(|&node| {
            if *full.get_kind(node) != SymKind::FPValue {
                return false;
            }
            let Some(outcome) = full.children(node).next() else {
                return false;
            };
            if *full.get_kind(outcome) != rounded {
                return false;
            }
            let children: Vec<_> = full.children(outcome).collect();
            children.len() == operands.len() + 1
                && matches!(
                    full.get_leaf_data(children[operands.len()]),
                    Some(SymPayload::Int(mode)) if mode.width() == 3 && mode.to_u64() == 0
                )
                && operands
                    .iter()
                    .zip(&children)
                    .all(|(&lhs, &rhs)| subgraphs_equal(candidate, lhs, full, rhs))
        }));
    }
    if shared.is_empty() {
        return false;
    }

    let mut proof = SemGraph::new();
    let source = proof.add_node(SymKind::Symbol);
    proof.set_leaf_data(source, SymPayload::SymbolId(0));
    let mut memo: HashMap<_, _> = shared.into_iter().map(|node| (node, source)).collect();
    let target = copy_result(full, full_value, &mut proof, &mut memo);
    let source_nan = node(&mut proof, SymKind::Ne, &[source, source]);
    let source_bits = node(&mut proof, SymKind::Bitcast, &[source]);
    let target_bits = node(&mut proof, SymKind::Bitcast, &[target]);
    let same_bits = node(&mut proof, SymKind::Eq, &[source_bits, target_bits]);
    let mask = constant(&mut proof, width, nan_mask);
    let target_class = node(&mut proof, SymKind::And, &[target_bits, mask]);
    let quiet_nan = node(&mut proof, SymKind::Eq, &[target_class, mask]);
    node(&mut proof, SymKind::If, &[source_nan, quiet_nan, same_bits]);

    let mut always = SemGraph::new();
    constant(&mut always, 1, 1);
    let mut inferred_symbols = vec![None; symbol_types.len()];
    for node in candidate.postorder(candidate_value) {
        if let Some(SymPayload::SymbolId(id)) = candidate.get_leaf_data(node)
            && let Some(slot) = inferred_symbols.get_mut(*id as usize)
        {
            *slot = Some(types[node.index()].clone());
        }
    }
    let mut proof_types = vec![SemType::Float(format.clone())];
    proof_types.extend(
        symbol_types
            .iter()
            .zip(inferred_symbols)
            .map(|(declared, inferred)| {
                declared
                    .clone()
                    .or(inferred)
                    .unwrap_or_else(|| SemType::bits(width))
            }),
    );
    SmtOracle.equivalent_typed(&proof, &always, &proof_types)
}

pub(super) fn value_refines(
    full: &SemGraph,
    candidate: &SemGraph,
    symbol_types: &[SemType],
    symbol_type_seeds: &[Option<SemType>],
) -> bool {
    let (Some(full_root), Some(candidate_root)) = (full.root(), candidate.root()) else {
        return false;
    };
    let mut full_value = value_result(full, full_root);
    let mut candidate_value = value_result(candidate, candidate_root);
    if matches!(
        candidate.get_kind(candidate_value),
        SymKind::SExt | SymKind::ZExt
    ) && candidate.get_kind(candidate_value) == full.get_kind(full_value)
        && subgraphs_equal(
            candidate,
            candidate.children(candidate_value).nth(1).unwrap(),
            full,
            full.children(full_value).nth(1).unwrap(),
        )
    {
        candidate_value = candidate.children(candidate_value).next().unwrap();
        full_value = full.children(full_value).next().unwrap();
    }
    if subgraphs_equal(candidate, candidate_value, full, full_value) {
        return true;
    }
    let mut candidate_numeric = SemGraph::new();
    tir_symbolic::sem::copy_subgraph(
        &mut candidate_numeric,
        candidate,
        candidate_value,
        &mut HashMap::new(),
    );
    let mut full_numeric = SemGraph::new();
    tir_symbolic::sem::copy_subgraph(&mut full_numeric, full, full_value, &mut HashMap::new());
    SmtOracle.refines_typed(&candidate_numeric, &full_numeric, symbol_types)
        || ieee_arithmetic_refines(full, candidate, symbol_type_seeds)
}

pub(super) fn event_refines(
    full: &SemGraph,
    candidate: &SemGraph,
    symbol_types: &[SemType],
    symbol_type_seeds: &[Option<SemType>],
) -> bool {
    let (Some(full_root), Some(candidate_root)) = (full.root(), candidate.root()) else {
        return false;
    };
    if *full.get_kind(full_root) != SymKind::StateResult
        || *candidate.get_kind(candidate_root) != SymKind::StateResult
    {
        return false;
    }
    let full_children: Vec<_> = full.children(full_root).collect();
    let candidate_children: Vec<_> = candidate.children(candidate_root).collect();
    if full_children.len() != candidate_children.len() {
        return false;
    }
    full_children[1..].iter().zip(&candidate_children[1..]).all(
        |(&full_state, &candidate_state)| {
            state_refines(full, full_state, candidate, candidate_state)
        },
    ) && value_refines(full, candidate, symbol_types, symbol_type_seeds)
}

fn value_result(graph: &SemGraph, root: NodeId) -> NodeId {
    if *graph.get_kind(root) == SymKind::StateResult {
        graph.children(root).next().unwrap()
    } else {
        root
    }
}

fn state_refines(
    full: &SemGraph,
    full_state: NodeId,
    candidate: &SemGraph,
    candidate_state: NodeId,
) -> bool {
    if *full.get_kind(full_state) != SymKind::StateIf {
        return subgraphs_equal(full, full_state, candidate, candidate_state);
    }
    let children: Vec<_> = full.children(full_state).collect();
    let [condition, trap, valid] = children.as_slice() else {
        return false;
    };
    *full.get_kind(*trap) == SymKind::StateTrap
        && guard_is_outside_state_domain(full, *condition)
        && subgraphs_equal(full, *valid, candidate, candidate_state)
}

fn guard_is_outside_state_domain(graph: &SemGraph, condition: NodeId) -> bool {
    use tir::sem::{StateFieldKind, StateResourceKind};

    if *graph.get_kind(condition) != SymKind::UGt {
        return false;
    }
    let children: Vec<_> = graph.children(condition).collect();
    let [read, limit] = children.as_slice() else {
        return false;
    };
    if *graph.get_kind(*read) != SymKind::StateRead {
        return false;
    }
    let read_children: Vec<_> = graph.children(*read).collect();
    let [_, resource, field] = read_children.as_slice() else {
        return false;
    };
    let (Some(resource), Some(field), Some(limit)) = (
        constant_value(graph, *resource),
        constant_value(graph, *field),
        constant_value(graph, *limit),
    ) else {
        return false;
    };
    StateResourceKind::from_code(resource)
        .zip(StateFieldKind::from_code(field))
        .and_then(|(resource, field)| resource.field_schema(field))
        .and_then(|schema| schema.maximum)
        .is_some_and(|maximum| maximum <= limit)
}

fn constant_value(graph: &SemGraph, node: NodeId) -> Option<u64> {
    match graph.get_leaf_data(node) {
        Some(SymPayload::Int(value)) => Some(value.to_u64()),
        _ => None,
    }
}

fn rounded_kind(kind: SymKind) -> Option<SymKind> {
    Some(match kind {
        SymKind::FAdd => SymKind::FAddRound,
        SymKind::FSub => SymKind::FSubRound,
        SymKind::FMul => SymKind::FMulRound,
        SymKind::FDiv => SymKind::FDivRound,
        SymKind::Sqrt => SymKind::SqrtRound,
        SymKind::Fma => SymKind::FmaRound,
        SymKind::FCvt => SymKind::FCvtRound,
        _ => return None,
    })
}

fn quiet_nan_mask(format: &FloatFormat) -> Option<(u32, u64)> {
    match (&format.exponent, &format.mantissa) {
        (Width::Const(8), Width::Const(23)) => Some((32, 0x7fc00000)),
        (Width::Const(11), Width::Const(52)) => Some((64, 0x7ff8000000000000)),
        _ => None,
    }
}

fn copy_result(
    full: &SemGraph,
    root: NodeId,
    proof: &mut SemGraph,
    memo: &mut HashMap<NodeId, NodeId>,
) -> NodeId {
    if let Some(&copied) = memo.get(&root) {
        return copied;
    }
    let children: Vec<_> = full
        .children(root)
        .map(|child| copy_result(full, child, proof, memo))
        .collect();
    let copied = node(proof, *full.get_kind(root), &children);
    if let Some(payload) = full.get_leaf_data(root) {
        let payload = match payload {
            SymPayload::SymbolId(id) => SymPayload::SymbolId(id + 1),
            payload => payload.clone(),
        };
        proof.set_leaf_data(copied, payload);
    }
    memo.insert(root, copied);
    copied
}

fn node(graph: &mut SemGraph, kind: SymKind, children: &[NodeId]) -> NodeId {
    let node = graph.add_node(kind);
    for &child in children {
        graph.add_edge(node, child);
    }
    node
}

fn constant(graph: &mut SemGraph, width: u32, value: u64) -> NodeId {
    let node = graph.add_node(SymKind::Constant);
    graph.set_leaf_data(node, SymPayload::Int(APInt::new(width, value)));
    node
}
