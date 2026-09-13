#[derive(Clone, Default)]
struct FixedRegisterOperands(HashMap<String, u16>);

type RegisterIndexCandidates = Vec<(String, u32, Vec<(u16, tir_adt::APInt)>)>;

fn remove_dead_fixed_operands(
    semantics: &mut InstructionSemantics,
    fixed: &FixedRegisterOperands,
) -> bool {
    use tir_graph::Dag;
    use tir_symbolic::lang::SymPayload;

    let contains = |graph: &tir_symbolic::sem::SemGraph, root, symbol| {
        graph.preorder(root).any(|node| {
            matches!(graph.get_leaf_data(node), Some(SymPayload::SymbolId(found)) if *found == symbol)
        })
    };
    for name in fixed.0.keys() {
        let Some(&symbol) = semantics.variable_symbols.get(name) else {
            continue;
        };
        if contains(&semantics.pattern, semantics.root, symbol)
            || semantics
                .guarded_semantics
                .as_ref()
                .is_some_and(|(graph, root)| contains(graph, *root, symbol))
        {
            return false;
        }
        semantics.variable_symbols.remove(name);
    }
    true
}

fn specialized_rule_features(
    files: &[ast::File],
    instruction_isas: &[String],
    behavior: &sem_expr_state::BehaviorGraph,
) -> Option<RuleFeatures> {
    use tir_graph::Dag;
    use tir_symbolic::lang::SymPayload;

    let live_symbols: HashSet<_> = behavior
        .graph
        .preorder(behavior.root)
        .filter_map(|node| match behavior.graph.get_leaf_data(node) {
            Some(sem_expr_state::BehaviorPayload::Value(SymPayload::SymbolId(symbol))) => {
                Some(*symbol)
            }
            _ => None,
        })
        .collect();
    let mut classes: BTreeSet<_> = behavior
        .register_symbols
        .iter()
        .filter_map(|((class, _), symbol)| live_symbols.contains(symbol).then_some(class.clone()))
        .collect();
    classes.extend(behavior.effect_nodes().filter_map(|node| {
        match behavior.effect_payload(node) {
            Some(sem_expr_state::EffectPayload::Assign {
                destination: sem_expr_state::Destination::FixedRegister { class, .. },
            }) => Some(class.clone()),
            _ => None,
        }
    }));

    let mut required = RuleFeatures::any(instruction_isas);
    for class in classes {
        let class_isas = &files
            .iter()
            .flat_map(|file| file.register_classes())
            .find(|candidate| candidate.name == class)?
            .for_isas;
        required = required.and(RuleFeatures::any(class_isas))?;
    }
    Some(required)
}

fn register_operand_specializations(
    behavior: &sem_expr_state::BehaviorGraph,
    operands: &[(String, Type)],
    register_indices: &HashMap<(String, String), u32>,
) -> Vec<(sem_expr_state::BehaviorGraph, FixedRegisterOperands)> {
    use tir_graph::Dag;
    use tir_symbolic::lang::{SymKind, SymPayload};

    let mut candidates: RegisterIndexCandidates = Vec::new();
    for (name, symbol) in &behavior.regnum_symbols {
        let Some(Type::Struct(class)) = operands
            .iter()
            .find_map(|(operand, ty)| (operand == name).then_some(ty))
        else {
            continue;
        };
        let valid: HashSet<u16> = register_indices
            .iter()
            .filter_map(|((candidate, _), index)| {
                (candidate == class)
                    .then(|| u16::try_from(*index).ok())
                    .flatten()
            })
            .collect();
        let mut indices = BTreeSet::new();
        for node in behavior.graph.preorder(behavior.root) {
            if !matches!(behavior.graph.get_node(node), SymKind::Eq | SymKind::Ne) {
                continue;
            }
            let children: Vec<_> = behavior.graph.children(node).collect();
            let [left, right] = children.as_slice() else {
                continue;
            };
            for (symbol_node, constant_node) in [(*left, *right), (*right, *left)] {
                let is_symbol = matches!(
                    behavior.graph.get_leaf_data(symbol_node),
                    Some(sem_expr_state::BehaviorPayload::Value(SymPayload::SymbolId(id)))
                        if id == symbol
                );
                if !is_symbol {
                    continue;
                }
                let Some((constant_graph, _)) = behavior.value_graph(constant_node) else {
                    continue;
                };
                let Some(value) = evaluate_integer(constant_graph, &HashMap::new()) else {
                    continue;
                };
                if let Ok(index) = u16::try_from(value.to_u64())
                    && valid.contains(&index)
                {
                    indices.insert((index, value));
                }
            }
        }
        if !indices.is_empty() {
            candidates.push((
                name.clone(),
                *symbol,
                indices.into_iter().collect::<Vec<_>>(),
            ));
        }
    }
    if candidates.len() != behavior.regnum_symbols.len() {
        return Vec::new();
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    if candidates.is_empty() {
        return Vec::new();
    }

    fn enumerate(
        behavior: &sem_expr_state::BehaviorGraph,
        candidates: &RegisterIndexCandidates,
        at: usize,
        fixed: &mut FixedRegisterOperands,
        values: &mut HashMap<u32, tir_adt::APInt>,
        out: &mut Vec<(sem_expr_state::BehaviorGraph, FixedRegisterOperands)>,
    ) {
        if at == candidates.len() {
            if let Some(specialized) = specialize_behavior(behavior, values) {
                out.push((specialized, fixed.clone()));
            }
            return;
        }
        let (name, symbol, choices) = &candidates[at];
        for (index, value) in choices {
            fixed.0.insert(name.clone(), *index);
            values.insert(*symbol, value.clone());
            enumerate(behavior, candidates, at + 1, fixed, values, out);
        }
        fixed.0.remove(name);
        values.remove(symbol);
    }

    let mut out = Vec::new();
    enumerate(
        behavior,
        &candidates,
        0,
        &mut FixedRegisterOperands::default(),
        &mut HashMap::new(),
        &mut out,
    );
    out
}

fn emit_instruction_value_rules(
    tables: &TargetTables<'_>,
    ctx: &InstrEmitCtx<'_>,
    semantics: Option<&InstructionSemantics>,
    trap_handler: Option<&ast::TrapHandler>,
    numeric_params: &HashMap<String, i64>,
    isa_param_values: &HashMap<String, i64>,
    out: &mut InstrOutputs<'_>,
) {
    if let Some(semantics) = semantics {
        emit_value_rules(
            tables,
            ctx,
            semantics,
            None,
            &RuleFeatures::any(&ctx.inst.for_isas),
            "",
            out,
        );
    }
    let Some(behavior) = sem_expr_state::lower_behavior(
        &ctx.inst.behavior,
        trap_handler,
        numeric_params,
        isa_param_values,
        &tables.register_index_map,
    ) else {
        return;
    };
    for (behavior, fixed_operands) in
        register_operand_specializations(&behavior, ctx.ops, &tables.register_index_map)
    {
        let live_defs = surviving_register_defs(&behavior, ctx.ops);
        let specialized_features =
            specialized_rule_features(tables.files, &ctx.inst.for_isas, &behavior);
        let Some(mut specialized) =
            analyze_fp_behavior_semantics(behavior, &live_defs, &tables.fp_register_roles)
        else {
            continue;
        };
        let Some(specialized_features) = specialized_features else {
            continue;
        };
        if !remove_dead_fixed_operands(&mut specialized, &fixed_operands) {
            continue;
        }
        let mut suffix: Vec<_> = fixed_operands.0.iter().collect();
        suffix.sort_by_key(|(name, _)| *name);
        let suffix = suffix
            .into_iter()
            .map(|(name, index)| format!("_{name}{index}"))
            .collect::<String>();
        emit_value_rules(
            tables,
            ctx,
            &specialized,
            Some(&fixed_operands),
            &specialized_features,
            &suffix,
            out,
        );
    }
}

fn evaluate_integer(
    mut graph: sem_expr_state::ValueGraph,
    supplied: &HashMap<u32, tir_adt::APInt>,
) -> Option<tir_adt::APInt> {
    use tir_graph::{Dag, MutDag};
    use tir_symbolic::lang::{SymPayload, Value};

    let mut arguments = Vec::new();
    let mut remapped = HashMap::new();
    let nodes: Vec<_> = graph.preorder(graph.root()?).collect();
    for node in nodes {
        match graph.get_leaf_data(node).cloned() {
            Some(SymPayload::Int(_)) => {}
            Some(SymPayload::SymbolId(symbol)) => {
                let value = supplied.get(&symbol)?.clone();
                let argument = *remapped.entry(symbol).or_insert_with(|| {
                    arguments.push(Value::Int(value));
                    u32::try_from(arguments.len() - 1).unwrap()
                });
                graph.set_leaf_data(node, SymPayload::SymbolId(argument));
            }
            Some(_) => return None,
            None if !foldable_kind(graph.get_node(node)) => return None,
            None => {}
        }
    }
    match tir_symbolic::lang::execute(&graph, &arguments) {
        Value::Int(value) => Some(value),
        _ => None,
    }
}

fn specialize_behavior(
    behavior: &sem_expr_state::BehaviorGraph,
    values: &HashMap<u32, tir_adt::APInt>,
) -> Option<sem_expr_state::BehaviorGraph> {
    use tir_graph::{Dag, MutDag};
    use tir_symbolic::lang::{SymKind, SymPayload};

    fn condition(
        behavior: &sem_expr_state::BehaviorGraph,
        node: tir_graph::NodeId,
        values: &HashMap<u32, tir_adt::APInt>,
    ) -> Option<bool> {
        let (source, _) = behavior.value_graph(node)?;
        evaluate_integer(source, values).map(|value| value.to_u64() != 0)
    }

    fn copy(
        behavior: &sem_expr_state::BehaviorGraph,
        node: tir_graph::NodeId,
        values: &HashMap<u32, tir_adt::APInt>,
        out: &mut sem_expr_state::UnifiedGraph,
        memo: &mut HashMap<usize, tir_graph::NodeId>,
    ) -> Option<tir_graph::NodeId> {
        if let Some(&known) = memo.get(&node.index()) {
            return Some(known);
        }
        let children: Vec<_> = behavior.graph.children(node).collect();
        if matches!(
            behavior.graph.get_node(node),
            SymKind::If | SymKind::StateIf
        ) && let Some(take_then) = children
            .first()
            .and_then(|child| condition(behavior, *child, values))
        {
            let chosen = if take_then {
                children.get(1)
            } else {
                children.get(2)
            };
            if let Some(chosen) = chosen {
                let copied = copy(behavior, *chosen, values, out, memo)?;
                memo.insert(node.index(), copied);
                return Some(copied);
            }
            if *behavior.graph.get_node(node) == SymKind::StateIf {
                let empty = out.add_node(SymKind::StateBlock);
                out.set_leaf_data(
                    empty,
                    sem_expr_state::BehaviorPayload::Effect(sem_expr_state::EffectPayload::None),
                );
                memo.insert(node.index(), empty);
                return Some(empty);
            }
            return None;
        }
        let copied_children: Vec<_> = children
            .into_iter()
            .map(|child| copy(behavior, child, values, out, memo))
            .collect::<Option<_>>()?;
        let substituted = matches!(
            behavior.graph.get_leaf_data(node),
            Some(sem_expr_state::BehaviorPayload::Value(SymPayload::SymbolId(symbol)))
                if values.contains_key(symbol)
        );
        let new = out.add_node(if substituted {
            SymKind::Constant
        } else {
            *behavior.graph.get_node(node)
        });
        match behavior.graph.get_leaf_data(node) {
            Some(sem_expr_state::BehaviorPayload::Value(SymPayload::SymbolId(symbol)))
                if values.contains_key(symbol) =>
            {
                out.set_leaf_data(
                    new,
                    sem_expr_state::BehaviorPayload::Value(SymPayload::Int(values[symbol].clone())),
                );
            }
            Some(payload) => out.set_leaf_data(new, payload.clone()),
            None => {}
        }
        memo.insert(node.index(), new);
        for child in copied_children {
            out.add_edge(new, child);
        }
        Some(new)
    }

    let mut graph = sem_expr_state::UnifiedGraph::new();
    let mut memo = HashMap::new();
    let root = copy(behavior, behavior.root, values, &mut graph, &mut memo)?;
    Some(sem_expr_state::BehaviorGraph {
        graph,
        root,
        variable_symbols: behavior.variable_symbols.clone(),
        register_symbols: behavior.register_symbols.clone(),
        regnum_symbols: behavior.regnum_symbols.clone(),
        let_symbols: behavior
            .let_symbols
            .iter()
            .filter_map(|(node, symbol)| memo.get(&node.index()).map(|node| (*node, *symbol)))
            .collect(),
    })
}

fn surviving_register_defs(
    behavior: &sem_expr_state::BehaviorGraph,
    operands: &[(String, Type)],
) -> Vec<String> {
    let names = register_operand_names(operands);
    behavior
        .effect_nodes()
        .filter_map(|node| match behavior.effect_payload(node) {
            Some(sem_expr_state::EffectPayload::Assign {
                destination: sem_expr_state::Destination::Ident(name),
            }) if names.contains(name.as_str()) => Some(name.clone()),
            _ => None,
        })
        .fold(Vec::new(), |mut defs, name| {
            if !defs.contains(&name) {
                defs.push(name);
            }
            defs
        })
}
