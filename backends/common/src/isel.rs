use std::collections::HashMap;

use tir::{
    Context, OpId, Operation, OperationRef, Pass, PassError, PassTarget, Rewriter, ValueId,
    attributes::AttributeValue,
    sem_expr::{APInt, Expr, simplify},
};

#[derive(Clone)]
pub struct IselNode {
    pub op: OperationRef,
    pub expr: Expr,
    pub leaf_values: HashMap<u32, ValueId>,
}

pub trait IselGraph {
    fn nodes(&self) -> &[IselNode];
}

pub struct OpSemanticGraph {
    nodes: Vec<IselNode>,
}

impl OpSemanticGraph {
    pub fn single(node: IselNode) -> Self {
        Self { nodes: vec![node] }
    }
}

impl IselGraph for OpSemanticGraph {
    fn nodes(&self) -> &[IselNode] {
        &self.nodes
    }
}

#[derive(Debug, Clone)]
pub struct RuleMatch {
    expr_bindings: HashMap<u32, Expr>,
    value_bindings: HashMap<u32, ValueId>,
}

impl RuleMatch {
    fn new(expr_bindings: HashMap<u32, Expr>, value_bindings: HashMap<u32, ValueId>) -> Self {
        Self {
            expr_bindings,
            value_bindings,
        }
    }

    pub fn expr_binding(&self, symbol: u32) -> Option<&Expr> {
        self.expr_bindings.get(&symbol)
    }

    pub fn value_binding(&self, symbol: u32) -> Option<ValueId> {
        self.value_bindings.get(&symbol).copied()
    }

    pub fn int_binding(&self, symbol: u32) -> Option<i64> {
        match self.expr_binding(symbol) {
            Some(Expr::Int(v)) => Some(v.to_u64() as i64),
            _ => None,
        }
    }
}

pub type RuleEmitter =
    fn(&Context, &OperationRef, &RuleMatch) -> Result<Box<dyn Operation>, PassError>;

#[derive(Clone)]
pub struct Rule {
    pub name: &'static str,
    pub pattern: Expr,
    pub cost: u32,
    pub emit: RuleEmitter,
}

pub struct Selection {
    pub rule_index: usize,
    pub m: RuleMatch,
}

pub trait IselAlgorithm: Send + Sync {
    fn select(&self, node: &IselNode, rules: &[Rule]) -> Option<Selection>;
}

pub struct GreedyBottomUp;

impl IselAlgorithm for GreedyBottomUp {
    fn select(&self, node: &IselNode, rules: &[Rule]) -> Option<Selection> {
        let mut best: Option<(usize, RuleMatch, u32)> = None;
        for (i, rule) in rules.iter().enumerate() {
            if let Some(m) = match_pattern(&rule.pattern, &node.expr, &node.leaf_values) {
                match &best {
                    Some((_, _, c)) if *c <= rule.cost => {}
                    _ => best = Some((i, m, rule.cost)),
                }
            }
        }
        best.map(|(rule_index, m, _)| Selection { rule_index, m })
    }
}

fn match_pattern(
    pattern: &Expr,
    candidate: &Expr,
    leaf_values: &HashMap<u32, ValueId>,
) -> Option<RuleMatch> {
    let mut expr_bindings: HashMap<u32, Expr> = HashMap::new();
    if !match_expr(pattern, candidate, &mut expr_bindings) {
        return None;
    }

    let mut value_bindings = HashMap::new();
    for (sym, bound) in &expr_bindings {
        if let Expr::Symbol(candidate_sym) = bound {
            if let Some(v) = leaf_values.get(candidate_sym) {
                value_bindings.insert(*sym, *v);
            }
        }
    }

    Some(RuleMatch::new(expr_bindings, value_bindings))
}

fn match_expr(pattern: &Expr, candidate: &Expr, bindings: &mut HashMap<u32, Expr>) -> bool {
    match pattern {
        Expr::Symbol(id) => {
            if let Some(existing) = bindings.get(id) {
                existing == candidate
            } else {
                bindings.insert(*id, candidate.clone());
                true
            }
        }
        Expr::Int(a) => matches!(candidate, Expr::Int(b) if a == b),
        Expr::Bool(a) => matches!(candidate, Expr::Bool(b) if a == b),
        Expr::Add(a1, a2) => match_binary(a1, a2, candidate, bindings, true, |c| match c {
            Expr::Add(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::Sub(a1, a2) => match_binary(a1, a2, candidate, bindings, false, |c| match c {
            Expr::Sub(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::Mul(a1, a2) => match_binary(a1, a2, candidate, bindings, true, |c| match c {
            Expr::Mul(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::Div(a1, a2) => match_binary(a1, a2, candidate, bindings, false, |c| match c {
            Expr::Div(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::ShiftLeft(a1, a2) => match_binary(a1, a2, candidate, bindings, false, |c| match c {
            Expr::ShiftLeft(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::ShiftRightLogic(a1, a2) => {
            match_binary(a1, a2, candidate, bindings, false, |c| match c {
                Expr::ShiftRightLogic(l, r) => Some((l.as_ref(), r.as_ref())),
                _ => None,
            })
        }
        Expr::ShiftRightArithmetic(a1, a2) => {
            match_binary(a1, a2, candidate, bindings, false, |c| match c {
                Expr::ShiftRightArithmetic(l, r) => Some((l.as_ref(), r.as_ref())),
                _ => None,
            })
        }
        Expr::And(a1, a2) => match_binary(a1, a2, candidate, bindings, true, |c| match c {
            Expr::And(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::Or(a1, a2) => match_binary(a1, a2, candidate, bindings, true, |c| match c {
            Expr::Or(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        Expr::Xor(a1, a2) => match_binary(a1, a2, candidate, bindings, true, |c| match c {
            Expr::Xor(l, r) => Some((l.as_ref(), r.as_ref())),
            _ => None,
        }),
        _ => pattern == candidate,
    }
}

fn match_binary<'a, F>(
    lhs: &Expr,
    rhs: &Expr,
    candidate: &'a Expr,
    bindings: &mut HashMap<u32, Expr>,
    commutative: bool,
    extract: F,
) -> bool
where
    F: Fn(&'a Expr) -> Option<(&'a Expr, &'a Expr)>,
{
    let Some((cand_lhs, cand_rhs)) = extract(candidate) else {
        return false;
    };

    let mut copy = bindings.clone();
    if match_expr(lhs, cand_lhs, &mut copy) && match_expr(rhs, cand_rhs, &mut copy) {
        *bindings = copy;
        return true;
    }

    if commutative {
        let mut copy = bindings.clone();
        if match_expr(lhs, cand_rhs, &mut copy) && match_expr(rhs, cand_lhs, &mut copy) {
            *bindings = copy;
            return true;
        }
    }

    false
}

fn substitute_symbols(expr: &Expr, bindings: &HashMap<u32, Expr>) -> Expr {
    match expr {
        Expr::Symbol(id) => bindings.get(id).cloned().unwrap_or(Expr::Symbol(*id)),
        Expr::Add(lhs, rhs) => Expr::Add(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::Sub(lhs, rhs) => Expr::Sub(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::Mul(lhs, rhs) => Expr::Mul(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::Div(lhs, rhs) => Expr::Div(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::ShiftLeft(lhs, rhs) => Expr::ShiftLeft(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::ShiftRightLogic(lhs, rhs) => Expr::ShiftRightLogic(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::ShiftRightArithmetic(lhs, rhs) => Expr::ShiftRightArithmetic(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::And(lhs, rhs) => Expr::And(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::Or(lhs, rhs) => Expr::Or(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        Expr::Xor(lhs, rhs) => Expr::Xor(
            Box::new(substitute_symbols(lhs, bindings)),
            Box::new(substitute_symbols(rhs, bindings)),
        ),
        _ => expr.clone(),
    }
}

fn build_sem_expr_for_op(
    context: &Context,
    op_ref: &OperationRef,
) -> Option<(Expr, HashMap<u32, ValueId>)> {
    let sem = op_ref.op().clone().as_dyn_op().semantic_expr()?;

    let mut value_to_def: HashMap<ValueId, OpId> = HashMap::new();
    if let Some(block) = op_ref.block() {
        for op_id in block.op_ids() {
            let op = context.get_op(op_id);
            for v in &op.results {
                value_to_def.insert(*v, op_id);
            }
        }
    }

    let mut next_symbol = 10_000u32;
    let mut leaf_values = HashMap::new();

    fn build_from_value(
        context: &Context,
        value: ValueId,
        value_to_def: &HashMap<ValueId, OpId>,
        next_symbol: &mut u32,
        leaf_values: &mut HashMap<u32, ValueId>,
    ) -> Expr {
        if let Some(def_op_id) = value_to_def.get(&value) {
            let def = context.get_op(*def_op_id);
            if def.name == "constant" {
                if let Some(attr) = def.attributes.iter().find(|a| a.name == "value") {
                    if let AttributeValue::Int(v) = &attr.value {
                        return Expr::Int(APInt::new_signed(64, *v));
                    }
                }
            }
            let dyn_op = def.clone().as_dyn_op();
            if let Some(sem_expr) = dyn_op.semantic_expr() {
                let mut op_bindings: HashMap<u32, Expr> = HashMap::new();
                for (idx, operand) in def.operands.iter().enumerate() {
                    let sub =
                        build_from_value(context, *operand, value_to_def, next_symbol, leaf_values);
                    op_bindings.insert(idx as u32, sub);
                }
                return simplify(substitute_symbols(&sem_expr, &op_bindings));
            }
        }

        let sym = *next_symbol;
        *next_symbol += 1;
        leaf_values.insert(sym, value);
        Expr::Symbol(sym)
    }

    let mut op_bindings: HashMap<u32, Expr> = HashMap::new();
    for (idx, operand) in op_ref.op().operands.iter().enumerate() {
        let expr = build_from_value(
            context,
            *operand,
            &value_to_def,
            &mut next_symbol,
            &mut leaf_values,
        );
        op_bindings.insert(idx as u32, expr);
    }

    let expr = simplify(substitute_symbols(&sem, &op_bindings));
    Some((expr, leaf_values))
}

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    algorithm: Box<dyn IselAlgorithm>,
    op_lowerings: Vec<OpLowering>,
}

pub type OpLowering = fn(&Context, &OperationRef, &mut Rewriter) -> Result<bool, PassError>;

impl InstructionSelectPass {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            algorithm: Box::new(GreedyBottomUp),
            op_lowerings: vec![],
        }
    }

    pub fn with_algorithm(mut self, algorithm: Box<dyn IselAlgorithm>) -> Self {
        self.algorithm = algorithm;
        self
    }

    pub fn with_op_lowering(mut self, lowering: OpLowering) -> Self {
        self.op_lowerings.push(lowering);
        self
    }
}

impl Pass for InstructionSelectPass {
    fn name(&self) -> &'static str {
        "instruction-select"
    }

    fn target(&self) -> PassTarget {
        PassTarget::Any
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        for lowering in &self.op_lowerings {
            if lowering(context, op, rewriter)? {
                return Ok(());
            }
        }

        if op.op().results.is_empty() {
            return Ok(());
        }
        if op.op().clone().as_dyn_op().semantic_expr().is_none() {
            return Ok(());
        }

        let Some((expr, leaf_values)) = build_sem_expr_for_op(context, op) else {
            return Ok(());
        };

        let graph = OpSemanticGraph::single(IselNode {
            op: op.clone(),
            expr,
            leaf_values,
        });
        let node = &graph.nodes()[0];

        if let Some(selection) = self.algorithm.select(node, &self.rules) {
            let rule = &self.rules[selection.rule_index];
            let new_op = (rule.emit)(context, op, &selection.m)?;
            rewriter.replace_op(op, new_op.as_ref())?;
        }

        Ok(())
    }
}
