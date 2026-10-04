use crate::ast::*;
use tir_symbolic::lang::{SymKind, op_kind};

impl Operator {
    /// The homogeneous integer operation whose matching laws this operator supports.
    pub fn algebraic_kind(&self) -> Option<SymKind> {
        let kind = match self {
            Self::Semantic(name) => op_kind(name)?,
            Self::Dialect { dialect, name } if dialect == "builtin" => match name.as_str() {
                "addi" => SymKind::Add,
                "muli" => SymKind::Mul,
                "andi" => SymKind::And,
                "ori" => SymKind::Or,
                "xori" => SymKind::Xor,
                _ => return None,
            },
            _ => return None,
        };
        kind.is_associative().then_some(kind)
    }
}

impl Rule {
    /// Validate selected laws and locate the maximal compatible operation patterns.
    pub fn algebraic_patterns(&self) -> Result<Vec<AlgebraicPattern>, String> {
        if self.match_options == MatchOptions::default() {
            return Ok(Vec::new());
        }
        if !matches!(self.kind, RuleKind::Equality(Direction::Forward)) {
            return Err("algebraic matching requires a forward equality rule".into());
        }
        if self.is_floating_point() {
            return Err("algebraic matching supports homogeneous integer operations only".into());
        }
        let mut patterns = Vec::new();
        collect(self, &self.lhs, &mut Vec::new(), &mut patterns)?;
        if patterns.is_empty() {
            return Err(
                "match options require an integer add, mul, and, or, or xor pattern".into(),
            );
        }
        Ok(patterns)
    }
}

fn collect(
    rule: &Rule,
    term: &Term,
    path: &mut Vec<usize>,
    out: &mut Vec<AlgebraicPattern>,
) -> Result<(), String> {
    let TermKind::Operation {
        operator,
        attributes,
        operands,
        dependencies,
    } = &term.kind
    else {
        return Ok(());
    };
    if operator.algebraic_kind().is_some() {
        if !attributes.is_empty() || !dependencies.is_empty() || operands.len() != 2 {
            return Err("algebraic operation patterns require two operands and no attributes or dependencies".into());
        }
        let mut leaves = Vec::new();
        flatten(term, operator, &term.ty, &mut leaves)?;
        let mut remainder = None;
        if rule.match_options.associative {
            for leaf in &leaves {
                if let TermKind::Binder { name, ty } = &leaf.kind
                    && !matches!(ty, Some(BindingType::Constant(_)))
                {
                    if occurrences(&rule.lhs, name) != 1 {
                        continue;
                    }
                    if remainder.is_some() {
                        return Err(
                            "associative pattern has more than one opaque remainder binder".into(),
                        );
                    }
                    if rule.guards.iter().any(|guard| expression_uses(guard, name)) {
                        return Err(format!(
                            "associative remainder `{name}` must be opaque and used only in the replacement"
                        ));
                    }
                    remainder = Some(name.clone());
                }
            }
        }
        let mut width = None;
        let types = term.ty.iter().chain(leaves.iter().flat_map(|leaf| {
            leaf.ty.iter().chain(match &leaf.kind {
                TermKind::Binder {
                    ty: Some(BindingType::Type(ty)),
                    ..
                } => Some(ty),
                _ => None,
            })
        }));
        for ty in types {
            let Some(actual) = integer_width(ty) else {
                return Err("algebraic operand binders must have integer types".into());
            };
            if !matches!(actual, Width::Any) {
                if let Some(expected) = &width
                    && expected != actual
                {
                    return Err("algebraic operand widths must agree".into());
                }
                width = Some(actual.clone());
            }
        }
        out.push(AlgebraicPattern {
            path: path.clone(),
            remainder,
            selector_order: selector_order(rule, &leaves),
        });
        // Different operation families below a selected leaf have their own pattern.
        for (index, child) in operands.iter().enumerate() {
            path.push(index);
            collect_below(rule, child, operator, path, out)?;
            path.pop();
        }
    } else {
        for (index, child) in operands.iter().chain(dependencies).enumerate() {
            path.push(index);
            collect(rule, child, path, out)?;
            path.pop();
        }
    }
    Ok(())
}

fn collect_below(
    rule: &Rule,
    term: &Term,
    parent: &Operator,
    path: &mut Vec<usize>,
    out: &mut Vec<AlgebraicPattern>,
) -> Result<(), String> {
    if let TermKind::Operation {
        operator, operands, ..
    } = &term.kind
        && operator == parent
    {
        for (index, child) in operands.iter().enumerate() {
            path.push(index);
            collect_below(rule, child, parent, path, out)?;
            path.pop();
        }
        Ok(())
    } else {
        collect(rule, term, path, out)
    }
}

fn flatten<'a>(
    term: &'a Term,
    root: &Operator,
    root_ty: &Option<Type>,
    leaves: &mut Vec<&'a Term>,
) -> Result<(), String> {
    if let TermKind::Operation {
        operator,
        attributes,
        operands,
        dependencies,
    } = &term.kind
        && operator == root
    {
        if !attributes.is_empty() || !dependencies.is_empty() || operands.len() != 2 {
            return Err("algebraic operation patterns require two operands and no attributes or dependencies".into());
        }
        if term.ty.is_some() && &term.ty != root_ty {
            return Err(
                "nested algebraic operation types must agree with their pattern root".into(),
            );
        }
        for child in operands {
            flatten(child, root, root_ty, leaves)?;
        }
    } else {
        leaves.push(term);
    }
    Ok(())
}

fn integer_width(ty: &Type) -> Option<&Width> {
    match ty {
        Type::Integer(width) => Some(width),
        _ => None,
    }
}

fn occurrences(term: &Term, name: &str) -> usize {
    match &term.kind {
        TermKind::Binder { name: found, .. } => usize::from(found == name),
        TermKind::Operation {
            operands,
            dependencies,
            ..
        } => operands
            .iter()
            .chain(dependencies)
            .map(|child| occurrences(child, name))
            .sum(),
        _ => 0,
    }
}

fn expression_uses(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::Name(found) => found == name,
        ExprKind::Call { args, .. } => args.iter().any(|arg| expression_uses(arg, name)),
        ExprKind::Unary { value, .. } => expression_uses(value, name),
        ExprKind::Binary { lhs, rhs, .. } => {
            expression_uses(lhs, name) || expression_uses(rhs, name)
        }
        ExprKind::Integer(_) => false,
    }
}

// This proves rule validity under a transposition, not identical extraction
// alternatives when the matcher chooses among residual witnesses.
fn selector_order(rule: &Rule, leaves: &[&Term]) -> Option<(String, String)> {
    if !rule.match_options.associative || !rule.match_options.commutative {
        return None;
    }
    let candidates: Vec<_> = leaves
        .iter()
        .filter_map(|term| {
            let TermKind::Binder {
                name,
                ty: Some(BindingType::Constant(width)),
            } = &term.kind
            else {
                return None;
            };
            (occurrences(&rule.lhs, name) == 1).then_some((name, width, &term.ty))
        })
        .collect();
    for (index, (a, width_a, ty_a)) in candidates.iter().enumerate() {
        for (b, width_b, ty_b) in &candidates[index + 1..] {
            if a == b || ty_a != ty_b {
                continue;
            }
            let pair = (a.as_str(), b.as_str());
            if !plain_lhs(&rule.lhs, pair) {
                continue;
            }
            let widths = match (width_a, width_b) {
                (None, None) => true,
                (Some(a), Some(b)) => normalized_expr(a, pair, false)
                    .is_some_and(|a| normalized_expr(b, pair, false).as_ref() == Some(&a)),
                _ => false,
            };
            if widths
                && rule.guards.iter().all(|guard| {
                    normalized_expr(guard, pair, false).is_some_and(|original| {
                        normalized_expr(guard, pair, true).as_ref() == Some(&original)
                    })
                })
                && normalized_term(&rule.rhs, pair, false).is_some_and(|original| {
                    normalized_term(&rule.rhs, pair, true).as_ref() == Some(&original)
                })
            {
                return Some(((*a).clone(), (*b).clone()));
            }
        }
    }
    None
}

fn renamed(name: &str, pair: (&str, &str), swap: bool) -> String {
    if swap && name == pair.0 {
        pair.1.into()
    } else if swap && name == pair.1 {
        pair.0.into()
    } else {
        name.into()
    }
}

fn normalized_expr(expr: &Expr, pair: (&str, &str), swap: bool) -> Option<Expr> {
    let kind = match &expr.kind {
        ExprKind::Integer(value) => ExprKind::Integer(*value),
        ExprKind::Name(name) => ExprKind::Name(renamed(name, pair, swap)),
        ExprKind::Call { name, args } => {
            // Extern calls have no declared symmetry or purity contract.
            if args
                .iter()
                .any(|arg| expression_uses(arg, pair.0) || expression_uses(arg, pair.1))
            {
                return None;
            }
            ExprKind::Call {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| normalized_expr(arg, pair, swap))
                    .collect::<Option<_>>()?,
            }
        }
        ExprKind::Unary { op, value } => ExprKind::Unary {
            op: *op,
            value: Box::new(normalized_expr(value, pair, swap)?),
        },
        ExprKind::Binary { op, lhs, rhs } => {
            let mut lhs = normalized_expr(lhs, pair, swap)?;
            let mut rhs = normalized_expr(rhs, pair, swap)?;
            if matches!(
                op,
                BinaryOp::Add
                    | BinaryOp::Multiply
                    | BinaryOp::BitAnd
                    | BinaryOp::BitOr
                    | BinaryOp::BitXor
                    | BinaryOp::Equal
                    | BinaryOp::NotEqual
            ) && format!("{:?}", lhs.kind) > format!("{:?}", rhs.kind)
            {
                std::mem::swap(&mut lhs, &mut rhs);
            }
            ExprKind::Binary {
                op: *op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            }
        }
    };
    Some(Expr {
        kind,
        span: (0..0).into(),
    })
}

fn normalized_term(term: &Term, pair: (&str, &str), swap: bool) -> Option<Term> {
    if term.ty.as_ref().is_some_and(|ty| type_uses(ty, pair)) {
        return None;
    }
    let kind = match &term.kind {
        // Class identity and kept instruction provenance are not scalar values.
        TermKind::Binder { name, .. } if name == pair.0 || name == pair.1 => return None,
        TermKind::Keep(_) => return None,
        TermKind::Binder { ty: Some(ty), .. } if binding_type_uses(ty, pair) => return None,
        TermKind::Operation {
            operator,
            attributes,
            operands,
            dependencies,
        } => {
            if attributes.iter().any(|attr| matches!(&attr.value, AttributeValue::Binder(name) if name == pair.0 || name == pair.1)) {
                return None;
            }
            if dependencies.iter().any(|child| {
                occurrences(child, pair.0) != 0
                    || occurrences(child, pair.1) != 0
                    || !plain_lhs(child, pair)
            }) {
                return None;
            }
            let mut attributes = attributes.clone();
            for attribute in &mut attributes {
                attribute.span = (0..0).into();
            }
            TermKind::Operation {
                operator: operator.clone(),
                attributes,
                operands: operands
                    .iter()
                    .map(|child| normalized_term(child, pair, swap))
                    .collect::<Option<_>>()?,
                dependencies: dependencies
                    .iter()
                    .map(|child| normalized_term(child, pair, swap))
                    .collect::<Option<_>>()?,
            }
        }
        TermKind::Value(expr) => TermKind::Value(normalized_expr(expr, pair, swap)?),
        TermKind::Constant { width, value } => {
            if expression_uses(width, pair.0) || expression_uses(width, pair.1) {
                return None;
            }
            TermKind::Constant {
                width: normalized_expr(width, pair, swap)?,
                value: normalized_expr(value, pair, swap)?,
            }
        }
        other => other.clone(),
    };
    Some(Term {
        kind,
        ty: term.ty.clone(),
        span: (0..0).into(),
    })
}

fn plain_lhs(term: &Term, pair: (&str, &str)) -> bool {
    if term.ty.as_ref().is_some_and(|ty| type_uses(ty, pair)) {
        return false;
    }
    let uses = |expr: &Expr| expression_uses(expr, pair.0) || expression_uses(expr, pair.1);
    match &term.kind {
        TermKind::Binder { ty: Some(ty), .. } => !binding_type_uses(ty, pair),
        TermKind::Operation { attributes, operands, dependencies, .. } => {
            !attributes.iter().any(|attr| matches!(&attr.value, AttributeValue::Binder(name) if name == pair.0 || name == pair.1))
                && operands.iter().all(|child| plain_lhs(child, pair))
                && dependencies.iter().all(|child| occurrences(child, pair.0) == 0 && occurrences(child, pair.1) == 0 && plain_lhs(child, pair))
        }
        TermKind::Value(expr) => !uses(expr),
        TermKind::Constant { width, value } => !uses(width) && !uses(value),
        TermKind::Keep(_) => false,
        _ => true,
    }
}

fn binding_type_uses(ty: &BindingType, pair: (&str, &str)) -> bool {
    match ty {
        BindingType::Type(ty) => type_uses(ty, pair),
        BindingType::Constant(width) => width
            .as_ref()
            .is_some_and(|width| expression_uses(width, pair.0) || expression_uses(width, pair.1)),
    }
}

fn type_uses(ty: &Type, pair: (&str, &str)) -> bool {
    match ty {
        Type::Integer(Width::Named(name)) | Type::Named(name) => name == pair.0 || name == pair.1,
        Type::ShapedFloat { shape, .. } => shape
            .iter()
            .any(|dim| expression_uses(dim, pair.0) || expression_uses(dim, pair.1)),
        _ => false,
    }
}
