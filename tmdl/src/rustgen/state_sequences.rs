struct StateSequenceCandidate {
    semantics: tir_symbolic::sem::SemGraph,
    root: tir_graph::NodeId,
    rule_key: String,
    emit_fn: proc_macro2::TokenStream,
    info: proc_macro2::TokenStream,
    features: RuleFeatures,
    result_class: String,
    parameters: Vec<(u32, String)>,
    register_inputs: Vec<(u32, String)>,
    constraints: Vec<proc_macro2::TokenStream>,
    registers: Vec<proc_macro2::TokenStream>,
    result: Option<proc_macro2::TokenStream>,
    imm_ranges: Vec<proc_macro2::TokenStream>,
}

fn specialize_parameters(
    files: &[ast::File],
    candidate: StateSequenceCandidate,
) -> Vec<StateSequenceCandidate> {
    use tir_graph::{Dag, MutDag};
    use tir_symbolic::lang::{SymKind, SymPayload};
    use tir_symbolic::sem::{CopyAction, copy_subgraph_with};

    if candidate.parameters.is_empty() {
        return vec![candidate];
    }
    let mut parameters = candidate.parameters.clone();
    parameters.sort_by(|left, right| left.1.cmp(&right.1));
    let mut combinations = vec![vec![]];
    for (_, name) in &parameters {
        let mut values: Vec<_> = isa_param_definers(files, name)
            .into_iter()
            .map(|(_, value)| value)
            .filter(|value| *value > 0)
            .collect();
        values.sort();
        values.dedup();
        combinations = combinations
            .into_iter()
            .flat_map(|prefix| {
                values.iter().map(move |value| {
                    let mut combination = prefix.clone();
                    combination.push(*value);
                    combination
                })
            })
            .collect();
    }
    combinations
        .into_iter()
        .filter_map(|values| {
            let mut features = candidate.features.clone();
            for ((_, name), value) in parameters.iter().zip(&values) {
                let definers = isa_param_definers(files, name);
                let equal = definers
                    .iter()
                    .filter(|(_, candidate)| candidate == value)
                    .map(|(name, _)| name.clone())
                    .collect();
                let greater = definers
                    .iter()
                    .filter(|(_, candidate)| candidate > value)
                    .map(|(name, _)| name.clone())
                    .collect();
                features = features.and(RuleFeatures {
                    any: vec![equal],
                    none: greater,
                })?;
            }
            let replacements: HashMap<_, _> = parameters
                .iter()
                .zip(&values)
                .map(|((symbol, _), value)| (*symbol, *value))
                .collect();
            let mut semantics = tir_symbolic::sem::SemGraph::new();
            let root = copy_subgraph_with(
                &mut semantics,
                &candidate.semantics,
                candidate.root,
                &mut HashMap::new(),
                &mut |out, node| match candidate.semantics.get_leaf_data(node) {
                    Some(SymPayload::SymbolId(symbol)) if replacements.contains_key(symbol) => {
                        let value = replacements[symbol] as u64;
                        let width = 64 - value.leading_zeros();
                        let constant = out.add_node(SymKind::Constant);
                        out.set_leaf_data(
                            constant,
                            SymPayload::Int(tir_adt::APInt::new(width.max(1), value)),
                        );
                        CopyAction::Replace(constant)
                    }
                    _ => CopyAction::Keep,
                },
            );
            let suffix = parameters
                .iter()
                .zip(values)
                .map(|((_, name), value)| format!("_{name}{value}"))
                .collect::<String>();
            Some(StateSequenceCandidate {
                semantics,
                root,
                rule_key: format!("{}{}", candidate.rule_key, suffix),
                emit_fn: candidate.emit_fn.clone(),
                info: candidate.info.clone(),
                features,
                result_class: candidate.result_class.clone(),
                parameters: Vec::new(),
                register_inputs: candidate.register_inputs.clone(),
                constraints: candidate.constraints.clone(),
                registers: candidate.registers.clone(),
                result: candidate.result.clone(),
                imm_ranges: candidate.imm_ranges.clone(),
            })
        })
        .collect()
}

struct StateTransition {
    value: tir_graph::NodeId,
    initial: tir_graph::NodeId,
    resource: tir_symbolic::lang::StateResourceKind,
    field: tir_symbolic::lang::StateFieldKind,
    width: u32,
    access: tir_symbolic::lang::StateAccessKind,
    assigned: tir_graph::NodeId,
}

enum StateSequenceRole {
    Reader(StateTransition),
    Writer {
        transition: StateTransition,
        input: u32,
    },
    Operator {
        transition: StateTransition,
        proposal: Box<tir_symbolic::sem::SemGraph>,
    },
}

fn constant(graph: &tir_symbolic::sem::SemGraph, node: tir_graph::NodeId) -> Option<u64> {
    use tir_graph::Dag;
    use tir_symbolic::lang::SymPayload;
    match graph.get_leaf_data(node)? {
        SymPayload::Int(value) => Some(value.to_u64()),
        _ => None,
    }
}

fn transition(candidate: &StateSequenceCandidate) -> Option<StateTransition> {
    use tir_graph::Dag;
    use tir_symbolic::lang::{StateAccessKind, StateFieldKind, StateResourceKind, SymKind, SymPayload};

    if *candidate.semantics.get_node(candidate.root) != SymKind::StateResult {
        return None;
    }
    let result: Vec<_> = candidate.semantics.children(candidate.root).collect();
    let [value, state] = result.as_slice() else {
        return None;
    };
    if *candidate.semantics.get_node(*state) != SymKind::StateAssign {
        return None;
    }
    let assign: Vec<_> = candidate.semantics.children(*state).collect();
    let [initial, resource, field, access, assigned] = assign.as_slice() else {
        return None;
    };
    if *candidate.semantics.get_node(*initial) != SymKind::Symbol {
        return None;
    }
    let Some(SymPayload::SymbolId(initial_symbol)) = candidate.semantics.get_leaf_data(*initial)
    else {
        return None;
    };
    let resource = StateResourceKind::from_code(constant(&candidate.semantics, *resource)?)?;
    let field = StateFieldKind::from_code(constant(&candidate.semantics, *field)?)?;
    if field == StateFieldKind::Whole {
        return None;
    }
    let width = resource.field_schema(field)?.bit_width?;
    let access = match constant(&candidate.semantics, *access)? {
        value if value == StateAccessKind::Read as u64 => StateAccessKind::Read,
        value if value == StateAccessKind::Change as u64 => StateAccessKind::Change,
        _ => return None,
    };
    let allowed: std::collections::HashSet<_> = std::iter::once(*initial_symbol)
        .chain(candidate.register_inputs.iter().map(|(symbol, _)| *symbol))
        .collect();
    if candidate.semantics.preorder(candidate.root).any(|node| {
        matches!(candidate.semantics.get_leaf_data(node), Some(SymPayload::SymbolId(symbol)) if !allowed.contains(symbol))
    }) {
        return None;
    }
    Some(StateTransition {
        value: *value,
        initial: *initial,
        resource,
        field,
        width,
        access,
        assigned: *assigned,
    })
}

fn field_read(
    graph: &tir_symbolic::sem::SemGraph,
    node: tir_graph::NodeId,
    state: &StateTransition,
) -> bool {
    use tir_graph::Dag;
    use tir_symbolic::lang::SymKind;

    let node = if matches!(graph.get_node(node), SymKind::ZExt | SymKind::SExt) {
        let children: Vec<_> = graph.children(node).collect();
        let [value, width] = children.as_slice() else {
            return false;
        };
        if constant(graph, *width).is_none_or(|width| width < u64::from(state.width)) {
            return false;
        }
        *value
    } else {
        node
    };
    if *graph.get_node(node) != SymKind::StateRead {
        return false;
    }
    let children: Vec<_> = graph.children(node).collect();
    let [initial, resource, field] = children.as_slice() else {
        return false;
    };
    tir_graph::subgraphs_equal(graph, *initial, graph, state.initial)
        && constant(graph, *resource) == Some(state.resource as u64)
        && constant(graph, *field) == Some(state.field as u64)
}

fn writer_input(
    candidate: &StateSequenceCandidate,
    state: &StateTransition,
) -> Option<u32> {
    use tir_graph::Dag;
    use tir_symbolic::lang::{SymKind, SymPayload};

    let [(input, _)] = candidate.register_inputs.as_slice() else {
        return None;
    };
    if *candidate.semantics.get_node(state.assigned) != SymKind::Extract {
        return None;
    }
    let children: Vec<_> = candidate.semantics.children(state.assigned).collect();
    let [value, high, low] = children.as_slice() else {
        return None;
    };
    matches!(candidate.semantics.get_leaf_data(*value), Some(SymPayload::SymbolId(symbol)) if symbol == input)
        .then_some(())?;
    (constant(&candidate.semantics, *high)? == u64::from(state.width - 1)
        && constant(&candidate.semantics, *low)? == 0)
        .then_some(*input)
}

fn classify_state_sequence_candidate(
    candidate: &StateSequenceCandidate,
) -> Option<StateSequenceRole> {
    use tir_graph::Dag;
    use tir_symbolic::lang::{StateAccessKind, SymKind};

    let state = transition(candidate)?;
    if candidate.register_inputs.is_empty()
        && state.access == StateAccessKind::Read
        && field_read(&candidate.semantics, state.value, &state)
        && *candidate.semantics.get_node(state.assigned) == SymKind::StateRead
        && field_read(&candidate.semantics, state.assigned, &state)
    {
        return Some(StateSequenceRole::Reader(state));
    }
    if state.access == StateAccessKind::Change
        && field_read(&candidate.semantics, state.value, &state)
        && let Some(input) = writer_input(candidate, &state)
    {
        return Some(StateSequenceRole::Writer {
            transition: state,
            input,
        });
    }
    if state.access != StateAccessKind::Change {
        return None;
    }
    let proposal = tir_symbolic::lang::value_observation_fallback(
        &candidate.semantics,
        candidate.root,
    )?;
    let root = proposal.root()?;
    let input_symbols: std::collections::HashSet<_> =
        candidate.register_inputs.iter().map(|(symbol, _)| *symbol).collect();
    if proposal.preorder(root).any(|node| {
        *proposal.get_node(node) == SymKind::StateRead
            || matches!(proposal.get_leaf_data(node), Some(tir_symbolic::lang::SymPayload::SymbolId(symbol)) if !input_symbols.contains(symbol))
    })
    {
        return None;
    }
    Some(StateSequenceRole::Operator {
        transition: state,
        proposal: Box::new(proposal),
    })
}

fn copy_transition(
    out: &mut tir_symbolic::sem::SemGraph,
    candidate: &StateSequenceCandidate,
    transition: &StateTransition,
    state: tir_graph::NodeId,
    input: Option<(u32, tir_graph::NodeId)>,
) -> (tir_graph::NodeId, tir_graph::NodeId) {
    use tir_graph::Dag;
    use tir_symbolic::lang::SymPayload;
    use tir_symbolic::sem::{CopyAction, copy_subgraph_with};

    let mut memo = HashMap::from([(transition.initial.index(), state)]);
    let initial_symbol = match candidate.semantics.get_leaf_data(transition.initial) {
        Some(SymPayload::SymbolId(symbol)) => *symbol,
        _ => unreachable!("classified transition has a symbolic initial state"),
    };
    let mut copy = |node| {
        copy_subgraph_with(
            out,
            &candidate.semantics,
            node,
            &mut memo,
            &mut |_, source| match candidate.semantics.get_leaf_data(source) {
                Some(SymPayload::SymbolId(symbol)) if *symbol == initial_symbol => {
                    CopyAction::Replace(state)
                }
                Some(SymPayload::SymbolId(symbol))
                    if input.is_some_and(|(input, _)| input == *symbol) =>
                {
                    CopyAction::Replace(input.unwrap().1)
                }
                _ => CopyAction::Keep,
            },
        )
    };
    let value = copy(transition.value);
    let state = candidate
        .semantics
        .children(candidate.root)
        .nth(1)
        .expect("classified StateResult has one state");
    let state = copy(state);
    (value, state)
}

fn emit_state_sequence_rules(
    files: &[ast::File],
    candidates: Vec<StateSequenceCandidate>,
    emitters: &mut Vec<proc_macro2::TokenStream>,
    rule_ids: &mut Vec<proc_macro2::Ident>,
) {
    use tir_graph::{Dag, MutDag};
    use tir_symbolic::lang::{SymKind, SymPayload};

    let candidates: Vec<_> = candidates
        .into_iter()
        .flat_map(|candidate| specialize_parameters(files, candidate))
        .collect();
    let classified: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            classify_state_sequence_candidate(candidate).map(|role| (index, role))
        })
        .collect();
    for (operator_index, operator) in &classified {
        let StateSequenceRole::Operator {
            transition: operator_state,
            proposal,
        } = operator
        else {
            continue;
        };
        let operator = &candidates[*operator_index];
        for (reader_index, reader) in &classified {
            let StateSequenceRole::Reader(reader_state) = reader else {
                continue;
            };
            if (reader_state.resource, reader_state.field)
                != (operator_state.resource, operator_state.field)
            {
                continue;
            }
            let reader = &candidates[*reader_index];
            for (writer_index, writer) in &classified {
                let StateSequenceRole::Writer {
                    transition: writer_state,
                    input: writer_input,
                } = writer
                else {
                    continue;
                };
                let writer = &candidates[*writer_index];
                if (writer_state.resource, writer_state.field)
                    != (operator_state.resource, operator_state.field)
                    || writer.register_inputs[0].1 != reader.result_class
                {
                    continue;
                }
                let Some(shared) = reader
                    .features
                    .clone()
                    .and(operator.features.clone())
                    .and_then(|features| features.and(writer.features.clone()))
                else {
                    continue;
                };

                let next_symbol = operator
                    .register_inputs
                    .iter()
                    .map(|(symbol, _)| *symbol)
                    .max()
                    .map_or(0, |symbol| symbol + 1);
                let mut guarded = tir_symbolic::sem::SemGraph::new();
                let initial = guarded.add_node(SymKind::Symbol);
                guarded.set_leaf_data(initial, SymPayload::SymbolId(next_symbol));
                let (saved, after_read) =
                    copy_transition(&mut guarded, reader, reader_state, initial, None);
                let (value, after_operator) = copy_transition(
                    &mut guarded,
                    operator,
                    operator_state,
                    after_read,
                    None,
                );
                let (_, after_write) = copy_transition(
                    &mut guarded,
                    writer,
                    writer_state,
                    after_operator,
                    Some((*writer_input, saved)),
                );
                let guarded_root = guarded.add_node(SymKind::StateResult);
                guarded.add_edge(guarded_root, value);
                guarded.add_edge(guarded_root, after_write);

                let proposal_root = proposal.root().expect("classified proposal has a root");
                let (pattern, pattern_root, forced_widths) =
                    tir_symbolic::lang::canonicalize_for_selection(
                        proposal.as_ref(),
                        proposal_root,
                        &HashSet::new(),
                    );
                let mut pattern_widths = tir_symbolic::lang::infer_widths(&pattern, |_| None);
                for (index, forced) in forced_widths.into_iter().enumerate() {
                    if forced.is_some() {
                        pattern_widths[index] = forced;
                    }
                }
                let (offset, typed) = intern_dag(&pattern, pattern_root, &pattern_widths);
                let pattern = SpecPattern {
                    offset,
                    typed,
                    float_width: None,
                };
                let guarded_widths = tir_symbolic::lang::infer_widths(&guarded, |_| None);
                let (offset, typed) = intern_dag(&guarded, guarded_root, &guarded_widths);
                let guarded = SpecPattern {
                    offset,
                    typed,
                    float_width: None,
                };
                let rule_key = format!(
                    "{}_via_{}_{}",
                    operator.rule_key, reader.rule_key, writer.rule_key
                );
                let writer_binding = quote! { &[(#writer_input, tir::backend::isel::StepBinding::Result(tir::backend::isel::StepResult { step: 0, result: 0 }))] };
                let steps = [
                    emit_rule_step(&reader.emit_fn, quote! { &[] }, quote! { &[] }),
                    emit_rule_step(&operator.emit_fn, quote! { &[] }, quote! { &[] }),
                    emit_rule_step(&writer.emit_fn, writer_binding, quote! { &[] }),
                ];
                let outputs = [emit_step_result(1, 0)];
                let emits = [reader.info.clone(), operator.info.clone(), writer.info.clone()];
                let (tokens, id) = emit_rule_spec(
                    &rule_key,
                    &rule_key,
                    &shared,
                    &pattern,
                    &emits,
                    quote! { tir::backend::isel::RuleKind::Value },
                    &steps,
                    &outputs,
                    &operator.constraints,
                    &operator.registers,
                    operator.result.clone(),
                    &operator.imm_ranges,
                    Some(&guarded),
                );
                emitters.push(tokens);
                rule_ids.push(id);
            }
        }
    }
}

impl StateSequenceCandidate {
    fn qualify(&mut self, module: &proc_macro2::Ident) {
        let emit_fn = &self.emit_fn;
        let info = &self.info;
        self.emit_fn = quote! { super::#module::#emit_fn };
        self.info = quote! { super::#module::#info };
    }
}
