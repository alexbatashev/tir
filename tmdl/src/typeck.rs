use std::collections::HashMap;

use chumsky::error::Rich;

use crate::{Span, ast, types::*};

type Diag = Rich<'static, String, Span>;

#[derive(Debug, Default, Clone)]
pub struct TypeCache {
    map: HashMap<(String, Span), Type>,
}

impl TypeCache {
    pub fn insert(&mut self, file: &str, span: Span, ty: Type) {
        self.map.insert((file.to_string(), span), ty);
    }

    pub fn get(&self, file: &str, span: Span) -> Option<&Type> {
        self.map.get(&(file.to_string(), span))
    }
}

/// Perform type checking/inference for all files after parsing and basic semantic checks.
/// Returns a cache of expression types and diagnostics.
pub fn check(files: &[ast::File]) -> (TypeCache, Vec<(String, Diag)>) {
    let mut cache = TypeCache::default();
    let mut diags: Vec<(String, Diag)> = Vec::new();
    let ctx = TypeCtx::new();

    // Index ISAs and templates by name
    let mut isas: HashMap<String, ast::Isa> = HashMap::new();
    let mut templates: HashMap<String, ast::Template> = HashMap::new();
    let mut reg_classes: HashMap<String, ast::RegisterClass> = HashMap::new();

    for f in files {
        for it in &f.items {
            match it {
                ast::Item::Isa(isa) => {
                    isas.insert(isa.name.clone(), isa.clone());
                }
                ast::Item::Template(t) => {
                    templates.insert(t.name.clone(), t.clone());
                }
                ast::Item::RegisterClass(rc) => {
                    reg_classes.insert(rc.name.clone(), rc.clone());
                }
                _ => {}
            }
        }
    }

    for f in files {
        for it in &f.items {
            if let ast::Item::Instruction(inst) = it {
                for isa_name in &inst.for_isas {
                    if let Some(isa_ext) = isas.get(isa_name) {
                        for base in resolve_base_isas(isa_ext, &isas) {
                            let env_builder = EnvBuilder::new(
                                &templates,
                                &reg_classes,
                                &isas,
                                isa_ext,
                                base,
                                inst,
                                &ctx,
                            );
                            let (params, operands, consts) = env_builder.build();

                            infer_expr(
                                &f.file_name,
                                &inst.behavior,
                                base.name.as_str(),
                                &params,
                                &operands,
                                &consts,
                                &ctx,
                                &mut cache,
                                &mut diags,
                            );
                        }
                    }
                }
            }
        }
    }

    (cache, diags)
}

struct EnvBuilder<'a> {
    templates: &'a HashMap<String, ast::Template>,
    reg_classes: &'a HashMap<String, ast::RegisterClass>,
    isas: &'a HashMap<String, ast::Isa>,
    isa_ext: &'a ast::Isa,
    isa_base: &'a ast::Isa,
    inst: &'a ast::Instruction,
    ctx: &'a TypeCtx,
}

impl<'a> EnvBuilder<'a> {
    fn new(
        templates: &'a HashMap<String, ast::Template>,
        reg_classes: &'a HashMap<String, ast::RegisterClass>,
        isas: &'a HashMap<String, ast::Isa>,
        isa_ext: &'a ast::Isa,
        isa_base: &'a ast::Isa,
        inst: &'a ast::Instruction,
        ctx: &'a TypeCtx,
    ) -> Self {
        Self {
            templates,
            reg_classes,
            isas,
            isa_ext,
            isa_base,
            inst,
            ctx,
        }
    }

    fn build(
        &self,
    ) -> (
        HashMap<String, (Type, Option<ast::Expr>)>,
        HashMap<String, Type>,
        HashMap<String, i64>,
    ) {
        let mut params: HashMap<String, (Type, Option<ast::Expr>)> = HashMap::new();
        let mut operands: HashMap<String, Type> = HashMap::new();
        let mut consts: HashMap<String, i64> = HashMap::new();

        // Base ISA params first (specific choice if Any)
        for (n, (t, def)) in collect_isa_params(self.isa_base, self.isas) {
            let ty: Type = t.clone().into();
            params.entry(n.clone()).or_insert((ty.clone(), def.clone()));
            if let Some(v) = def.as_ref().and_then(|e| eval_int(e, &consts)) {
                consts.insert(n.clone(), v);
            }
        }

        // Extension ISA params next (may override)
        for (n, (t, def)) in &self.isa_ext.parameters {
            let ty: Type = t.clone().into();
            params.insert(n.clone(), (ty.clone(), def.clone()));
            if let Some(v) = def.as_ref().and_then(|e| eval_int(e, &consts)) {
                consts.insert(n.clone(), v);
            }
        }

        // Template chain params/operands
        let mut chain: Vec<ast::Template> = Vec::new();
        collect_template_chain(
            self.inst.parent_template.as_deref(),
            self.templates,
            &mut chain,
        );
        for t in &chain {
            for (n, (ty, def)) in &t.params {
                let ty: Type = ty.clone().into();
                params.entry(n.clone()).or_insert((ty.clone(), def.clone()));
                if let Some(v) = def.as_ref().and_then(|e| eval_int(e, &consts)) {
                    consts.entry(n.clone()).or_insert(v);
                }
            }
            for (n, ty) in &t.operands {
                operands.insert(n.clone(), self.resolve_operand_type(ty, &consts));
            }
        }

        // Instruction params/operands override
        for (n, (ty, def)) in &self.inst.params {
            let ty: Type = ty.clone().into();
            params.insert(n.clone(), (ty.clone(), def.clone()));
            if let Some(v) = def.as_ref().and_then(|e| eval_int(e, &consts)) {
                consts.insert(n.clone(), v);
            }
        }
        for (n, ty) in &self.inst.operands {
            operands.insert(n.clone(), self.resolve_operand_type(ty, &consts));
        }

        (params, operands, consts)
    }

    fn resolve_operand_type(&self, ty: &ast::Type, consts: &HashMap<String, i64>) -> Type {
        match ty {
            ast::Type::Struct(rc_name) => {
                if let Some(sz) = reg_width(rc_name, self.reg_classes, consts) {
                    Type::Bits(sz)
                } else {
                    Type::Bits(self.ctx.fresh_size_var())
                }
            }
            _ => ty.clone().into(),
        }
    }
}

fn collect_template_chain(
    name: Option<&str>,
    templates: &HashMap<String, ast::Template>,
    out: &mut Vec<ast::Template>,
) {
    if let Some(n) = name {
        if let Some(t) = templates.get(n) {
            if let Some(parent) = &t.parent_template {
                collect_template_chain(Some(parent), templates, out);
            }
            out.push(t.clone());
        }
    }
}

fn collect_isa_params(
    isa: &ast::Isa,
    all_isas: &HashMap<String, ast::Isa>,
) -> Vec<(String, (ast::Type, Option<ast::Expr>))> {
    let mut collected = Vec::new();

    // include required ISA params first
    if let Some(req) = &isa.requires {
        match req {
            ast::IsaRequirement::Single(n) => {
                if let Some(base) = all_isas.get(n) {
                    collected.extend(collect_isa_params(base, all_isas));
                }
            }
            ast::IsaRequirement::Any(list) | ast::IsaRequirement::All(list) => {
                // conservative: include params from each option; later duplicates are ignored
                for n in list {
                    if let Some(base) = all_isas.get(n) {
                        collected.extend(collect_isa_params(base, all_isas));
                    }
                }
            }
        }
    }

    collected.extend(isa.parameters.iter().map(|(k, v)| (k.clone(), v.clone())));
    collected
}

fn resolve_base_isas<'a>(
    isa_ext: &'a ast::Isa,
    all_isas: &'a HashMap<String, ast::Isa>,
) -> Vec<&'a ast::Isa> {
    if let Some(req) = &isa_ext.requires {
        let mut v = Vec::new();
        match req {
            ast::IsaRequirement::Single(n) => {
                if let Some(base) = all_isas.get(n) {
                    v.push(base);
                }
            }
            ast::IsaRequirement::Any(list) | ast::IsaRequirement::All(list) => {
                for n in list {
                    if let Some(base) = all_isas.get(n) {
                        v.push(base);
                    }
                }
            }
        }
        if v.is_empty() {
            v.push(isa_ext);
        }
        v
    } else {
        vec![isa_ext]
    }
}

/// Given a register class name, try to resolve its value width from its parameters.
fn reg_width(
    rc_name: &str,
    reg_classes: &HashMap<String, ast::RegisterClass>,
    consts: &HashMap<String, i64>,
) -> Option<Size> {
    let rc = reg_classes.get(rc_name)?;
    if let Some((ty, default)) = rc.parameters.get("WIDTH") {
        match ty {
            ast::Type::Integer => {
                if let Some(expr) = default {
                    if let Some(v) = eval_int(expr, consts) {
                        if v >= 0 && v <= u16::MAX as i64 {
                            return Some(Size::Const(v as u16));
                        }
                    }
                }
            }
            ast::Type::Bits(w) => return Some(Size::Const(*w)),
            _ => {}
        }
    }
    None
}

fn infer_expr(
    file: &str,
    e: &ast::Expr,
    base_isa: &str,
    params: &HashMap<String, (Type, Option<ast::Expr>)>,
    operands: &HashMap<String, Type>,
    consts: &HashMap<String, i64>,
    ctx: &TypeCtx,
    cache: &mut TypeCache,
    diags: &mut Vec<(String, Diag)>,
) -> Type {
    match e {
        ast::Expr::Ident(id) => {
            if let Some((t, _)) = params.get(&id.name) {
                let ty = t.clone();
                cache.insert(file, id.span, ty.clone());
                ty
            } else if let Some(t) = operands.get(&id.name) {
                let ty = t.clone();
                cache.insert(file, id.span, ty.clone());
                ty
            } else {
                diags.push((
                    file.to_string(),
                    Rich::custom(
                        id.span,
                        format!("[base {}] Unknown identifier '{}'", base_isa, id.name),
                    ),
                ));
                let ty = ctx.fresh_type_var();
                cache.insert(file, id.span, ty.clone());
                ty
            }
        }
        ast::Expr::Field(f) => {
            if let ast::Expr::Ident(base) = &*f.base {
                if base.name == "self" {
                    if let Some((t, _)) = params.get(&f.member) {
                        let ty = t.clone();
                        cache.insert(file, f.span, ty.clone());
                        return ty;
                    } else if consts.contains_key(&f.member) {
                        // If it's a known constant, treat as Integer.
                        let ty = Type::Integer;
                        cache.insert(file, f.span, ty.clone());
                        return ty;
                    }
                }
            }
            diags.push((
                file.to_string(),
                Rich::custom(
                    f.span,
                    format!("[base {}] Unknown field access", base_isa),
                ),
            ));
            let ty = ctx.fresh_type_var();
            cache.insert(file, f.span, ty.clone());
            ty
        }
        ast::Expr::Slice(slc) => {
            let base = infer_expr(
                file,
                &slc.base,
                base_isa,
                params,
                operands,
                consts,
                ctx,
                cache,
                diags,
            );
            match base {
                Type::Bits(_w) => {
                    let width = if slc.end > slc.start {
                        slc.end - slc.start
                    } else {
                        0
                    };
                    let ty = Type::Bits(Size::Const(width));
                    cache.insert(file, slc.span, ty.clone());
                    ty
                }
                other => {
                        diags.push((
                            file.to_string(),
                            Rich::custom(
                                slc.span,
                                format!("[base {}] Cannot slice type {:?}; bits<N> required", base_isa, other),
                            ),
                        ));
                    let ty = ctx.fresh_type_var();
                    cache.insert(file, slc.span, ty.clone());
                    ty
                }
            }
        }
        ast::Expr::IndexAccess(idx) => {
            let base = infer_expr(
                file,
                &idx.base,
                base_isa,
                params,
                operands,
                consts,
                ctx,
                cache,
                diags,
            );
            match base {
                Type::Bits(_) => {
                    let ty = Type::Bits(Size::Const(1));
                    cache.insert(file, idx.span, ty.clone());
                    ty
                }
                other => {
                        diags.push((
                            file.to_string(),
                            Rich::custom(
                                idx.span,
                                format!("[base {}] Indexing not supported on type {:?}", base_isa, other),
                            ),
                        ));
                    let ty = ctx.fresh_type_var();
                    cache.insert(file, idx.span, ty.clone());
                    ty
                }
            }
        }
        ast::Expr::Binary(b) => {
            use ast::BinOp::*;
            let lt = infer_expr(
                file, &b.lhs, base_isa, params, operands, consts, ctx, cache, diags,
            );
            let rt = infer_expr(
                file, &b.rhs, base_isa, params, operands, consts, ctx, cache, diags,
            );

            let res = match b.op {
                Add | Sub | Div | BitwiseAnd | BitwiseOr | BitwiseXor => {
                    match (lt.clone(), rt.clone()) {
                        (Type::Bits(a), Type::Bits(bw)) => match unify_size(a, bw.clone()) {
                            Ok(_) => Type::Bits(bw),
                            Err(_) => {
                                diags.push((
                                    file.to_string(),
                                    Rich::custom(
                                        b.span,
                                        format!("[base {}] Bit widths must match for this operation", base_isa),
                                    ),
                                ));
                                ctx.fresh_type_var()
                            }
                        },
                        (Type::Integer, Type::Integer) => Type::Integer,
                        _ => {
                            diags.push((
                                file.to_string(),
                                    Rich::custom(
                                        b.span,
                                        format!(
                                            "[base {}] Operands must both be bits<N> or Integer",
                                            base_isa
                                        ),
                                    ),
                                ));
                            ctx.fresh_type_var()
                        }
                    }
                }
                Mul => match (lt.clone(), rt.clone()) {
                    (Type::Bits(a), Type::Bits(bw)) => {
                        let sz = simplify_size(Size::Add(Box::new(a), Box::new(bw)));
                        Type::Bits(sz)
                    }
                    (Type::Integer, Type::Integer) => Type::Integer,
                    (Type::Bits(a), Type::Integer) | (Type::Integer, Type::Bits(a)) => {
                        Type::Bits(a)
                    }
                    _ => Type::Integer,
                },
                ShiftLeftLogical | ShiftRightLogical | ShiftRightArithmetic => lt.clone(),
            };
            cache.insert(file, b.span, res.clone());
            res
        }
        ast::Expr::BuiltinFunction(_bf) => ctx.fresh_type_var(),
        ast::Expr::Call(c) => match &*c.callee {
            ast::Expr::BuiltinFunction(ast::BuiltinFunction::Extract) => {
                if c.arguments.len() != 3 {
                    diags.push((
                        file.to_string(),
                        Rich::custom(
                            c.span,
                            "extract expects 3 arguments: extract(bits, hi, lo)".to_string(),
                        ),
                    ));
                    let ty = ctx.fresh_type_var();
                    cache.insert(file, c.span, ty.clone());
                    return ty;
                }
                let val_ty = infer_expr(
                    file,
                    &c.arguments[0],
                    base_isa,
                    params,
                    operands,
                    consts,
                    ctx,
                    cache,
                    diags,
                );
                let hi_ty = infer_expr(
                    file,
                    &c.arguments[1],
                    base_isa,
                    params,
                    operands,
                    consts,
                    ctx,
                    cache,
                    diags,
                );
                let lo_ty = infer_expr(
                    file,
                    &c.arguments[2],
                    base_isa,
                    params,
                    operands,
                    consts,
                    ctx,
                    cache,
                    diags,
                );

                let mut ok = true;
                if !matches!(val_ty, Type::Bits(_)) {
                    ok = false;
                    diags.push((
                        file.to_string(),
                        Rich::custom(
                            c.span,
                            format!("extract first argument must be bits<N>, found {:?}", val_ty),
                        ),
                    ));
                }

                if !matches!(hi_ty, Type::Integer | Type::Bits(_))
                    || !matches!(lo_ty, Type::Integer | Type::Bits(_))
                {
                        ok = false;
                        diags.push((
                            file.to_string(),
                            Rich::custom(
                                c.span,
                                format!("[base {}] hi/lo must be integers", base_isa),
                            ),
                        ));
                    }

                let res_ty = if ok {
                    let hi = eval_int(&c.arguments[1], consts);
                    let lo = eval_int(&c.arguments[2], consts);
                    if let (Some(hi), Some(lo)) = (hi, lo) {
                        if hi < lo {
                            diags.push((
                                file.to_string(),
                                Rich::custom(c.span, "extract hi must be >= lo".to_string()),
                            ));
                        }
                        let width_val = (hi - lo + 1) as u16;
                        Type::Bits(Size::Const(width_val))
                    } else {
                        Type::Bits(simplify_size(ctx.fresh_size_var()))
                    }
                } else {
                    ctx.fresh_type_var()
                };
                cache.insert(file, c.span, res_ty.clone());
                res_ty
            }
            ast::Expr::BuiltinFunction(ast::BuiltinFunction::Clamp) => {
                if c.arguments.len() != 3 {
                    diags.push((
                        file.to_string(),
                        Rich::custom(
                            c.span,
                            "clamp expects 3 arguments: clamp(bits, lo, hi)".to_string(),
                        ),
                    ));
                    let ty = ctx.fresh_type_var();
                    cache.insert(file, c.span, ty.clone());
                    return ty;
                }
                let val_ty = infer_expr(
                    file,
                    &c.arguments[0],
                    base_isa,
                    params,
                    operands,
                    consts,
                    ctx,
                    cache,
                    diags,
                );
                let lo_ty = infer_expr(
                    file,
                    &c.arguments[1],
                    base_isa,
                    params,
                    operands,
                    consts,
                    ctx,
                    cache,
                    diags,
                );
                let hi_ty = infer_expr(
                    file,
                    &c.arguments[2],
                    base_isa,
                    params,
                    operands,
                    consts,
                    ctx,
                    cache,
                    diags,
                );

                let mut ok = true;
                let res_ty = if let Type::Bits(width) = val_ty.clone() {
                        if !matches!(lo_ty, Type::Integer | Type::Bits(_))
                            || !matches!(hi_ty, Type::Integer | Type::Bits(_))
                        {
                            ok = false;
                            diags.push((
                                file.to_string(),
                                Rich::custom(
                                    c.span,
                                    format!("[base {}] clamp bounds must be integers", base_isa),
                                ),
                            ));
                        }
                    Type::Bits(width)
                } else {
                    ok = false;
                    diags.push((
                        file.to_string(),
                        Rich::custom(c.span, "clamp first argument must be bits<N>".to_string()),
                    ));
                    ctx.fresh_type_var()
                };
                if ok {
                    cache.insert(file, c.span, res_ty.clone());
                }
                res_ty
            }
            _ => {
                diags.push((
                    file.to_string(),
                    Rich::custom(c.span, "Only builtin functions are supported".to_string()),
                ));
                let ty = ctx.fresh_type_var();
                cache.insert(file, c.span, ty.clone());
                ty
            }
        },
        ast::Expr::Assign(a) => {
            let rhs_ty = infer_expr(
                file,
                &a.value,
                base_isa,
                params,
                operands,
                consts,
                ctx,
                cache,
                diags,
            );
            if let Some(dst_ty) = operands
                .get(&a.dest)
                .or_else(|| params.get(&a.dest).map(|(t, _)| t))
            {
                let dst = dst_ty.clone();
                match unify(dst.clone(), rhs_ty.clone()) {
                    Ok(_) => {}
                    Err(_) => {
                                diags.push((
                                    file.to_string(),
                                    Rich::custom(
                                        a.span,
                                        format!(
                                            "[base {}] Type mismatch in assignment: {:?} := {:?}",
                                            base_isa, dst, rhs_ty
                                        ),
                                    ),
                                ));
                            }
                }
            } else {
                diags.push((
                    file.to_string(),
                    Rich::custom(a.span, format!("Unknown assignment target '{}'", a.dest)),
                ));
            }
            cache.insert(file, a.span, rhs_ty.clone());
            rhs_ty
        }
        ast::Expr::Block(b) => {
            let mut last_ty = ctx.fresh_type_var();
            for stmt in &b.stmts {
                last_ty = infer_expr(
                    file, stmt, base_isa, params, operands, consts, ctx, cache, diags,
                );
            }
            if b.last_expr_return {
                cache.insert(file, b.span, last_ty.clone());
                last_ty
            } else {
                let ty = ctx.fresh_type_var();
                cache.insert(file, b.span, ty.clone());
                ty
            }
        }
        ast::Expr::If(iff) => {
            let _cond_ty = infer_expr(
                file, &iff.cond, base_isa, params, operands, consts, ctx, cache, diags,
            );
            let then_ty = infer_expr(
                file, &iff.then, base_isa, params, operands, consts, ctx, cache, diags,
            );
            let else_ty = iff
                .else_
                .as_ref()
                .map(|e| infer_expr(file, e, base_isa, params, operands, consts, ctx, cache, diags));
            let res_ty = else_ty.unwrap_or(then_ty.clone());
            cache.insert(file, iff.span, res_ty.clone());
            res_ty
        }
        ast::Expr::Lit(ast::Lit::Int(li)) => {
            let ty = Type::Integer;
            cache.insert(file, li.span, ty.clone());
            ty
        }
        ast::Expr::Lit(ast::Lit::Str(ls)) => {
            let ty = Type::String;
            cache.insert(file, ls.span, ty.clone());
            ty
        }
        ast::Expr::Invalid => ctx.fresh_type_var(),
    }
}

fn eval_int(e: &ast::Expr, consts: &HashMap<String, i64>) -> Option<i64> {
    match e {
        ast::Expr::Lit(ast::Lit::Int(li)) => li.value().parse().ok(),
        ast::Expr::Ident(id) => consts.get(&id.name).copied(),
        ast::Expr::Field(f) => {
            if let ast::Expr::Ident(base) = &*f.base {
                if base.name == "self" {
                    return consts.get(&f.member).copied();
                }
            }
            None
        }
        ast::Expr::Binary(b) => {
            let l = eval_int(&b.lhs, consts)?;
            let r = eval_int(&b.rhs, consts)?;
            use ast::BinOp::*;
            match b.op {
                Add => Some(l + r),
                Sub => Some(l - r),
                Mul => Some(l * r),
                Div => Some(l / r),
                BitwiseAnd => Some(l & r),
                BitwiseOr => Some(l | r),
                BitwiseXor => Some(l ^ r),
                ShiftLeftLogical => Some(l << r),
                ShiftRightLogical | ShiftRightArithmetic => Some(l >> r),
            }
        }
        _ => None,
    }
}
