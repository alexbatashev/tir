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

#[derive(Clone)]
enum SExpr {
    Atom(String),
    List(Vec<SExpr>),
}

fn parse_sexpr(input: &str) -> Option<SExpr> {
    fn parse_list(tokens: &[char], pos: &mut usize) -> Option<SExpr> {
        if *pos >= tokens.len() || tokens[*pos] != '(' {
            return None;
        }
        *pos += 1;
        let mut items = Vec::new();
        loop {
            while *pos < tokens.len() && tokens[*pos].is_whitespace() {
                *pos += 1;
            }
            if *pos >= tokens.len() {
                return None;
            }
            if tokens[*pos] == ')' {
                *pos += 1;
                break;
            }
            if tokens[*pos] == '(' {
                items.push(parse_list(tokens, pos)?);
                continue;
            }
            let start = *pos;
            while *pos < tokens.len()
                && !tokens[*pos].is_whitespace()
                && tokens[*pos] != '('
                && tokens[*pos] != ')'
            {
                *pos += 1;
            }
            items.push(SExpr::Atom(tokens[start..*pos].iter().collect()));
        }
        Some(SExpr::List(items))
    }

    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    let expr = parse_list(&chars, &mut pos)?;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    if pos == chars.len() { Some(expr) } else { None }
}

fn sexpr_to_sem_expr(expr: &SExpr, operand_bindings: &HashMap<String, Expr>) -> Option<Expr> {
    match expr {
        SExpr::Atom(a) => {
            if let Some(v) = operand_bindings.get(a) {
                return Some(v.clone());
            }
            if let Ok(i) = a.parse::<i64>() {
                return Some(Expr::Int(APInt::new_signed(64, i)));
            }
            None
        }
        SExpr::List(items) => {
            let [SExpr::Atom(op), a, b] = items.as_slice() else {
                return None;
            };
            let lhs = sexpr_to_sem_expr(a, operand_bindings)?;
            let rhs = sexpr_to_sem_expr(b, operand_bindings)?;
            Some(match op.as_str() {
                "add" => Expr::Add(Box::new(lhs), Box::new(rhs)),
                "sub" => Expr::Sub(Box::new(lhs), Box::new(rhs)),
                "mul" => Expr::Mul(Box::new(lhs), Box::new(rhs)),
                "div" => Expr::Div(Box::new(lhs), Box::new(rhs)),
                "and" => Expr::And(Box::new(lhs), Box::new(rhs)),
                "or" => Expr::Or(Box::new(lhs), Box::new(rhs)),
                "xor" => Expr::Xor(Box::new(lhs), Box::new(rhs)),
                "shl" => Expr::ShiftLeft(Box::new(lhs), Box::new(rhs)),
                "lshr" => Expr::ShiftRightLogic(Box::new(lhs), Box::new(rhs)),
                "ashr" => Expr::ShiftRightArithmetic(Box::new(lhs), Box::new(rhs)),
                _ => return None,
            })
        }
    }
}

fn extract_sem_rhs<'a>(sem: &'a SExpr) -> Option<&'a SExpr> {
    // Expected: (set result <expr>)
    let SExpr::List(items) = sem else {
        return None;
    };
    let [SExpr::Atom(set_kw), SExpr::Atom(_dst), rhs] = items.as_slice() else {
        return None;
    };
    if set_kw == "set" { Some(rhs) } else { None }
}

fn build_sem_expr_for_op(
    context: &Context,
    op_ref: &OperationRef,
) -> Option<(Expr, HashMap<u32, ValueId>)> {
    let sem = op_ref.op().clone().as_dyn_op().semantic_expr()?;
    let sexpr = parse_sexpr(sem)?;
    let rhs = extract_sem_rhs(&sexpr)?;

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
            if let Some(sem) = dyn_op.semantic_expr() {
                let operand_names = dyn_op.operand_names();
                let mut op_bindings: HashMap<String, Expr> = HashMap::new();
                for (idx, name) in operand_names.iter().enumerate() {
                    if idx < def.operands.len() {
                        let sub = build_from_value(
                            context,
                            def.operands[idx],
                            value_to_def,
                            next_symbol,
                            leaf_values,
                        );
                        op_bindings.insert((*name).to_string(), sub);
                    }
                }
                if let Some(sexpr) = parse_sexpr(sem) {
                    if let Some(rhs) = extract_sem_rhs(&sexpr) {
                        if let Some(expr) = sexpr_to_sem_expr(rhs, &op_bindings) {
                            return simplify(expr);
                        }
                    }
                }
            }
        }

        let sym = *next_symbol;
        *next_symbol += 1;
        leaf_values.insert(sym, value);
        Expr::Symbol(sym)
    }

    let dyn_op = op_ref.op().clone().as_dyn_op();
    let operand_names = dyn_op.operand_names();
    let mut op_bindings: HashMap<String, Expr> = HashMap::new();
    for (idx, name) in operand_names.iter().enumerate() {
        if idx < op_ref.op().operands.len() {
            let v = op_ref.op().operands[idx];
            let expr = build_from_value(
                context,
                v,
                &value_to_def,
                &mut next_symbol,
                &mut leaf_values,
            );
            op_bindings.insert((*name).to_string(), expr);
        }
    }

    let expr = simplify(sexpr_to_sem_expr(rhs, &op_bindings)?);
    Some((expr, leaf_values))
}

pub struct InstructionSelectPass {
    rules: Vec<Rule>,
    algorithm: Box<dyn IselAlgorithm>,
}

impl InstructionSelectPass {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            algorithm: Box::new(GreedyBottomUp),
        }
    }

    pub fn with_algorithm(mut self, algorithm: Box<dyn IselAlgorithm>) -> Self {
        self.algorithm = algorithm;
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
