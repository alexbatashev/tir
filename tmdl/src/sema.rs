use std::collections::{HashMap, HashSet};

use chumsky::error::Rich;

use crate::{Span, ast};

type Diag = Rich<'static, String, Span>;

fn check_isas(
    file_name: &str,
    names: &[String],
    span: Span,
    isas: &HashMap<String, Span>,
    diags: &mut Vec<(String, Diag)>,
) {
    for n in names {
        if !isas.contains_key(n) {
            diags.push((
                file_name.to_string(),
                Rich::custom(span, format!("Unknown ISA '{}': not defined", n)),
            ));
        }
    }
}

pub fn analyze(files: Vec<ast::File>) -> Vec<(String, Diag)> {
    let mut diags: Vec<(String, Diag)> = Vec::new();

    // Index items by name
    let mut isas: HashMap<String, Span> = HashMap::new();
    let mut reg_classes: HashMap<String, Span> = HashMap::new();
    let mut templates: HashMap<String, (ast::Template, Span)> = HashMap::new();
    let mut template_owner: HashMap<String, String> = HashMap::new();
    let mut instructions: Vec<ast::Instruction> = Vec::new();

    for f in &files {
        for it in &f.items {
            match it {
                ast::Item::Isa(isa) => {
                    isas.insert(isa.name.clone(), isa.span);
                }
                ast::Item::RegisterClass(rc) => {
                    reg_classes.insert(rc.name.clone(), rc.span);
                }
                ast::Item::Template(t) => {
                    templates.insert(t.name.clone(), (t.clone(), t.span));
                    template_owner.insert(t.name.clone(), f.file_name.clone());
                }
                ast::Item::Instruction(i) => {
                    instructions.push(i.clone());
                }
            }
        }
    }

    // Detect cyclic inheritance among templates
    {
        #[derive(Copy, Clone, PartialEq, Eq)]
        enum Mark {
            Unvisited,
            Visiting,
            Done,
        }

        let mut mark: HashMap<String, Mark> = HashMap::new();
        for k in templates.keys() {
            mark.insert(k.clone(), Mark::Unvisited);
        }
        let mut stack: Vec<String> = Vec::new();

        fn dfs(
            name: &str,
            templates: &HashMap<String, (ast::Template, Span)>,
            owner: &HashMap<String, String>,
            mark: &mut HashMap<String, Mark>,
            stack: &mut Vec<String>,
            diags: &mut Vec<(String, Rich<'static, String, Span>)>,
        ) {
            mark.insert(name.to_string(), Mark::Visiting);
            stack.push(name.to_string());
            if let Some((t, sp)) = templates.get(name) {
                if let Some(parent) = &t.parent_template {
                    if let Some(m) = mark.get(parent) {
                        if *m == Mark::Unvisited {
                            dfs(parent, templates, owner, mark, stack, diags);
                        } else if *m == Mark::Visiting {
                            // cycle detected: find parent in stack
                            if let Some(pos) = stack.iter().position(|n| n == parent) {
                                let mut cycle = stack[pos..].to_vec();
                                cycle.push(parent.clone());
                                let path = cycle.join(" -> ");
                                let file = owner
                                    .get(name)
                                    .cloned()
                                    .unwrap_or_else(|| "<unknown>".to_string());
                                diags.push((
                                    file,
                                    Rich::custom(
                                        *sp,
                                        format!("Cyclic template inheritance: {}", path),
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
            stack.pop();
            mark.insert(name.to_string(), Mark::Done);
        }

        for name in templates.keys().cloned().collect::<Vec<_>>() {
            if mark.get(&name) == Some(&Mark::Unvisited) {
                dfs(
                    &name,
                    &templates,
                    &template_owner,
                    &mut mark,
                    &mut stack,
                    &mut diags,
                );
            }
        }
    }

    // Helper: resolve template lineage (root -> leaf)
    fn collect_template_chain<'a>(
        name: &str,
        templates: &'a HashMap<String, (ast::Template, Span)>,
        chain: &mut Vec<&'a ast::Template>,
        seen: &mut HashSet<String>,
    ) {
        if let Some((t, _)) = templates.get(name) {
            if let Some(parent) = &t.parent_template {
                if seen.insert(parent.clone()) {
                    collect_template_chain(parent, templates, chain, seen);
                }
            }
            chain.push(t);
        }
    }

    // Helper: operands and params for template/instruction
    fn resolve_operands_for_instruction(
        inst: &ast::Instruction,
        templates: &HashMap<String, (ast::Template, Span)>,
    ) -> HashMap<String, ast::Type> {
        let mut result = HashMap::new();
        if let Some(parent) = &inst.parent_template {
            let mut chain = Vec::new();
            let mut seen = HashSet::new();
            collect_template_chain(parent, templates, &mut chain, &mut seen);
            for t in chain {
                for (k, v) in &t.operands {
                    result.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in &inst.operands {
            result.insert(k.clone(), v.clone());
        }
        result
    }

    fn resolve_params_for_template(
        tmpl: &ast::Template,
        templates: &HashMap<String, (ast::Template, Span)>,
    ) -> HashMap<String, (ast::Type, Option<ast::Expr>)> {
        let mut out = HashMap::new();
        if let Some(parent) = &tmpl.parent_template {
            let mut chain = Vec::new();
            let mut seen = HashSet::new();
            collect_template_chain(parent, templates, &mut chain, &mut seen);
            for t in chain {
                for (k, v) in &t.params {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in &tmpl.params {
            out.insert(k.clone(), v.clone());
        }
        out
    }

    fn resolve_operands_for_template(
        tmpl: &ast::Template,
        templates: &HashMap<String, (ast::Template, Span)>,
    ) -> HashMap<String, ast::Type> {
        let mut out = HashMap::new();
        if let Some(parent) = &tmpl.parent_template {
            let mut chain = Vec::new();
            let mut seen = HashSet::new();
            collect_template_chain(parent, templates, &mut chain, &mut seen);
            for t in chain {
                for (k, v) in &t.operands {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in &tmpl.operands {
            out.insert(k.clone(), v.clone());
        }
        out
    }

    fn resolve_params_for_instruction(
        inst: &ast::Instruction,
        templates: &HashMap<String, (ast::Template, Span)>,
    ) -> HashMap<String, (ast::Type, Option<ast::Expr>)> {
        let mut out = HashMap::new();
        if let Some(parent) = &inst.parent_template {
            let mut chain = Vec::new();
            let mut seen = HashSet::new();
            collect_template_chain(parent, templates, &mut chain, &mut seen);
            for t in chain {
                for (k, v) in &t.params {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in &inst.params {
            out.insert(k.clone(), v.clone());
        }
        out
    }

    // Validate RegisterClass, Template, Isa references
    for f in &files {
        let current_file = &f.file_name;
        for it in &f.items {
            match it {
                ast::Item::Isa(isa) => {
                    // requires -> must refer to existing isas
                    if let Some(req) = &isa.requires {
                        match req {
                            ast::IsaRequirement::Single(n) => check_isas(
                                current_file,
                                &vec![n.clone()],
                                isa.span,
                                &isas,
                                &mut diags,
                            ),
                            ast::IsaRequirement::Any(v) | ast::IsaRequirement::All(v) => {
                                check_isas(current_file, v, isa.span, &isas, &mut diags)
                            }
                        }
                    }
                }
                ast::Item::RegisterClass(rc) => {
                    check_isas(current_file, &rc.for_isas, rc.span, &isas, &mut diags);
                }
                ast::Item::Template(t) => {
                    // parent template
                    if let Some(p) = &t.parent_template {
                        if !templates.contains_key(p) {
                            diags.push((
                                current_file.to_string(),
                                Rich::custom(t.span, format!("Unknown parent template '{}'", p)),
                            ));
                        }
                    }
                    check_isas(current_file, &t.for_isas, t.span, &isas, &mut diags);

                    // operand types must be valid; Struct name must be an existing RegisterClass
                    for (_name, ty) in &t.operands {
                        if let ast::Type::Struct(s) = ty {
                            if !reg_classes.contains_key(s) {
                                diags.push((
                                    current_file.to_string(),
                                    Rich::custom(
                                        t.span,
                                        format!("Unknown register class '{}' in operands", s),
                                    ),
                                ));
                            }
                        }
                    }

                    // encoding expressions name resolution
                    let params = resolve_params_for_template(t, &templates);
                    let ops = resolve_operands_for_template(t, &templates);
                    for arm in &t.encoding {
                        check_expr(current_file, &arm.value, &params, &ops, &mut diags);
                    }
                }
                ast::Item::Instruction(inst) => {
                    // parent template
                    if let Some(p) = &inst.parent_template {
                        if !templates.contains_key(p) {
                            diags.push((
                                current_file.to_string(),
                                Rich::custom(inst.span, format!("Unknown parent template '{}'", p)),
                            ));
                        }
                    }
                    check_isas(current_file, &inst.for_isas, inst.span, &isas, &mut diags);

                    // operand types must be valid
                    for (_name, ty) in &inst.operands {
                        if let ast::Type::Struct(s) = ty {
                            if !reg_classes.contains_key(s) {
                                diags.push((
                                    current_file.to_string(),
                                    Rich::custom(
                                        inst.span,
                                        format!("Unknown register class '{}' in operands", s),
                                    ),
                                ));
                            }
                        }
                    }

                    // encoding expressions name resolution
                    let params = resolve_params_for_instruction(inst, &templates);
                    let ops = resolve_operands_for_instruction(inst, &templates);
                    for arm in &inst.encoding {
                        check_expr(current_file, &arm.value, &params, &ops, &mut diags);
                    }

                    // behavior must assign to a known operand and reference only known operands
                    match &inst.behavior {
                        ast::Expr::Assign(a) => {
                            if !ops.contains_key(&a.dest) {
                                diags.push((
                                    current_file.to_string(),
                                    Rich::custom(
                                        a.span,
                                        format!(
                                            "Unknown assignment destination '{}' in behavior",
                                            a.dest
                                        ),
                                    ),
                                ));
                            }
                            // Type-check assignment compatibility when possible
                            let val_ty = check_expr(
                                current_file,
                                &a.value,
                                &HashMap::new(),
                                &ops,
                                &mut diags,
                            );
                            if let (Some(dst_ty), Some(src_ty)) = (ops.get(&a.dest), val_ty) {
                                if let Err(msg) = assignment_compatible(dst_ty, &src_ty) {
                                    diags.push((
                                        current_file.to_string(),
                                        Rich::custom(a.span, msg),
                                    ));
                                }
                            }
                        }
                        other => {
                            // For now only single assignment or block of assignments supported by generators
                            // Check identifiers inside anyway
                            check_expr(current_file, other, &HashMap::new(), &ops, &mut diags);
                        }
                    }
                }
            }
        }
    }

    diags
}

fn check_expr(
    file_name: &str,
    e: &ast::Expr,
    params: &HashMap<String, (ast::Type, Option<ast::Expr>)>,
    operands: &HashMap<String, ast::Type>,
    diags: &mut Vec<(String, Rich<'static, String, Span>)>,
) -> Option<ast::Type> {
    match e {
        ast::Expr::Ident(id) => {
            if let Some((t, _)) = params.get(&id.name) {
                Some(t.clone())
            } else if let Some(t) = operands.get(&id.name) {
                Some(t.clone())
            } else {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(id.span, format!("Unknown identifier '{}'", id.name)),
                ));
                None
            }
        }
        ast::Expr::Field(f) => {
            // only support self.<param>
            if let ast::Expr::Ident(base) = &*f.base {
                if base.name == "self" {
                    if let Some((t, _)) = params.get(&f.member) {
                        return Some(t.clone());
                    } else {
                        diags.push((
                            file_name.to_string(),
                            Rich::custom(
                                f.span,
                                format!("Unknown member 'self.{}' (no such parameter)", f.member),
                            ),
                        ));
                    }
                } else {
                    diags.push((
                        file_name.to_string(),
                        Rich::custom(
                            f.span,
                            "Only 'self.<param>' field access is supported".to_string(),
                        ),
                    ));
                }
            } else {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(f.span, "Unsupported field access expression".to_string()),
                ));
            }
            None
        }
        ast::Expr::Slice(s) => {
            if let Some(base_ty) = check_expr(file_name, &s.base, params, operands, diags) {
                match base_ty {
                    ast::Type::Bits(w) => {
                        // treat end as exclusive upper bound
                        let end_ex = s.end;
                        if end_ex > w || s.start >= w || s.start > end_ex {
                            diags.push((
                                file_name.to_string(),
                                Rich::custom(
                                    s.span,
                                    format!(
                                        "Slice [{}..{}] out of bounds for bits<{}>",
                                        s.start, s.end, w
                                    ),
                                ),
                            ));
                        }
                        let width = if end_ex > s.start {
                            end_ex - s.start
                        } else {
                            0
                        };
                        Some(ast::Type::Bits(width))
                    }
                    _ => {
                        diags.push((
                            file_name.to_string(),
                            Rich::custom(
                                s.span,
                                "Slice/index only supported on bits<N>".to_string(),
                            ),
                        ));
                        None
                    }
                }
            } else {
                None
            }
        }
        ast::Expr::IndexAccess(s) => {
            if let Some(base_ty) = check_expr(file_name, &s.base, params, operands, diags) {
                match base_ty {
                    ast::Type::Bits(w) => {
                        if s.index >= w {
                            diags.push((
                                file_name.to_string(),
                                Rich::custom(
                                    s.span,
                                    format!("Index [{}] out of bounds for bits<{}>", s.index, w),
                                ),
                            ));
                        }
                        Some(ast::Type::Bits(1))
                    }
                    _ => {
                        diags.push((
                            file_name.to_string(),
                            Rich::custom(
                                s.span,
                                "Slice/index only supported on bits<N>".to_string(),
                            ),
                        ));
                        None
                    }
                }
            } else {
                None
            }
        }
        ast::Expr::Binary(b) => {
            let lt = check_expr(file_name, &b.lhs, params, operands, diags);
            let rt = check_expr(file_name, &b.rhs, params, operands, diags);

            // Helper to report and return None
            let mut fail = |msg: String| {
                diags.push((file_name.to_string(), Rich::custom(b.span, msg)));
                None
            };

            match (lt, rt) {
                (Some(ast::Type::String), _) | (_, Some(ast::Type::String)) => {
                    fail("String type is not supported in binary operations".to_string())
                }
                (Some(ast::Type::Struct(_)), _) | (_, Some(ast::Type::Struct(_))) => {
                    fail("Register values not supported in binary operations".to_string())
                }
                (Some(l), Some(r)) => {
                    use ast::BinOp::*;
                    use ast::Type::*;
                    match b.op {
                        Add | Sub | Mul | Div => match (l, r) {
                            (Integer, Integer) => Some(Integer),
                            (Bits(w1), Bits(w2)) => {
                                if w1 == w2 {
                                    Some(Bits(w1))
                                } else {
                                    fail(format!(
                                        "Mismatched bit widths: bits<{}> {} bits<{}>",
                                        w1,
                                        op_name(b.op.clone()),
                                        w2
                                    ))
                                }
                            }
                            (Bits(w), Integer) | (Integer, Bits(w)) => Some(Bits(w)),
                            _ => fail("Arithmetic expects numeric operands".to_string()),
                        },
                        BitwiseAnd | BitwiseOr | BitwiseXor => match (l, r) {
                            (Integer, Integer) => Some(Integer),
                            (Bits(w1), Bits(w2)) => {
                                if w1 == w2 {
                                    Some(Bits(w1))
                                } else {
                                    fail(format!(
                                        "Mismatched bit widths for bitwise op: bits<{}> {} bits<{}>",
                                        w1,
                                        op_name(b.op.clone()),
                                        w2
                                    ))
                                }
                            }
                            (Bits(w), Integer) | (Integer, Bits(w)) => Some(Bits(w)),
                            _ => fail("Bitwise expects integer/bits operands".to_string()),
                        },
                        ShiftLeftLogical | ShiftRightLogical | ShiftRightArithmetic => {
                            match (l, r) {
                                (Bits(w), Integer) | (Bits(w), Bits(_)) => Some(Bits(w)),
                                (Integer, Integer) => Some(Integer),
                                (Integer, Bits(_)) => Some(Integer),
                                _ => fail(
                                    "Shift expects lhs numeric and rhs integer/bits".to_string(),
                                ),
                            }
                        }
                    }
                }
                _ => None,
            }
        }
        ast::Expr::Assign(_)
        | ast::Expr::Block(_)
        | ast::Expr::Call(_)
        | ast::Expr::If(_)
        | ast::Expr::Invalid => None,
        ast::Expr::Lit(ast::Lit::Int(_)) => Some(ast::Type::Integer),
        ast::Expr::Lit(ast::Lit::Str(_)) => Some(ast::Type::String),
    }
}

fn op_name(op: ast::BinOp) -> &'static str {
    use ast::BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        BitwiseAnd => "&",
        BitwiseOr => "|",
        BitwiseXor => "^",
        ShiftLeftLogical => "<<",
        ShiftRightLogical => ">>",
        ShiftRightArithmetic => ">>>",
    }
}

fn assignment_compatible(dst: &ast::Type, src: &ast::Type) -> Result<(), String> {
    use ast::Type::*;
    match (dst, src) {
        (String, String) => Ok(()),
        (Integer, Integer) => Ok(()),
        (Bits(wd), Bits(ws)) => {
            if wd == ws {
                Ok(())
            } else {
                Err(format!("Cannot assign bits<{}> to bits<{}>", ws, wd))
            }
        }
        (Bits(_), Integer) => Ok(()),
        (Struct(_), Integer) | (Struct(_), Bits(_)) => Ok(()), // allow writing numeric into a register
        _ => Err(format!("Incompatible assignment: {:?} := {:?}", dst, src)),
    }
}
