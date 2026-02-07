use std::collections::HashMap;
use std::io::Write;

use crate::ast::{self, Instruction, Item};
use crate::error::TMDLError;
use crate::utils::resolve_operands_for_instruction;

const REG_INDEX_WIDTH: u16 = 5;
const REG_VALUE_WIDTH: u16 = 64;

pub fn generate_smtlib(
    dialect: &str,
    files: Vec<ast::File>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    let item_cache = {
        let mut cache = HashMap::new();
        for f in &files {
            for item in &f.items {
                cache.insert(item.name().to_string(), item);
            }
        }
        cache
    };

    writeln!(output, "{}", HEADER)?;
    build_state(&files, &mut output)?;
    build_instructions(dialect, &item_cache, &files, &mut output)?;
    Ok(())
}

fn build_state(files: &[ast::File], output: &mut Box<dyn Write>) -> Result<(), TMDLError> {
    let reg_classes = files
        .iter()
        .flat_map(|file| {
            file.items
                .iter()
                .filter_map(|item: &Item| item.as_register_class().cloned())
        })
        .collect::<Vec<_>>();

    let mut fields = Vec::new();
    for rc in &reg_classes {
        let name = rc.name.to_lowercase();
        fields.push(format!(
            "({} (Array (_ BitVec {}) (_ BitVec {})))",
            name, REG_INDEX_WIDTH, REG_VALUE_WIDTH
        ));
    }
    fields.push(format!("(pc (_ BitVec {}))", REG_VALUE_WIDTH));

    writeln!(
        output,
        "(declare-datatypes () ((TMDLState (mk-TMDLState {}))))",
        fields.join(" ")
    )?;

    for rc in &reg_classes {
        let name = rc.name.to_lowercase();
        writeln!(
            output,
            "\n(define-fun read_{name} ((st TMDLState) (r (_ BitVec {idx_width}))) (_ BitVec {val_width})\n  (ite (= r (_ bv0 {idx_width}))\n    (_ bv0 {val_width})\n    (select ({name} st) r)))",
            idx_width = REG_INDEX_WIDTH,
            val_width = REG_VALUE_WIDTH
        )?;

        let mut fields: Vec<String> = Vec::new();
        for rc2 in &reg_classes {
            let n2 = rc2.name.to_lowercase();
            if n2 == name {
                fields.push(format!("(store ({} st) r val)", n2,));
            } else {
                fields.push(format!("({} st)", n2));
            }
        }
        fields.push("(pc st)".to_string());
        writeln!(
            output,
            "\n(define-fun write_{name} ((st TMDLState) (r (_ BitVec {idx_width})) (val (_ BitVec {val_width}))) TMDLState\n  (ite (= r (_ bv0 {idx_width}))\n    st\n    (mk-TMDLState {fields})))",
            idx_width = REG_INDEX_WIDTH,
            val_width = REG_VALUE_WIDTH,
            fields = fields.join(" ")
        )?;
    }

    let mut fields: Vec<String> = Vec::new();
    for rc in &reg_classes {
        let name = rc.name.to_lowercase();
        fields.push(format!("({} st)", name));
    }
    fields.push("val".to_string());
    writeln!(
        output,
        "\n(define-fun write_pc ((st TMDLState) (val (_ BitVec {val_width}))) TMDLState\n  (mk-TMDLState {fields}))",
        val_width = REG_VALUE_WIDTH,
        fields = fields.join(" ")
    )?;

    Ok(())
}

fn build_instructions<'a, 'cache: 'a>(
    dialect: &str,
    item_cache: &HashMap<String, &'cache Item>,
    files: &'a [ast::File],
    output: &mut Box<dyn Write>,
) -> Result<(), TMDLError> {
    let instructions = files
        .iter()
        .flat_map(|file| {
            file.items
                .iter()
                .filter_map(|item: &Item| item.as_instruction())
        })
        .collect::<Vec<_>>();

    let mut instruction_variants = vec![];
    let mut encode_arms = vec![];
    let mut execute_arms = vec![];

    for i in &instructions {
        let name = i.name.to_lowercase();
        let uppercase_name = name.to_uppercase();

        let operands = resolve_operands_for_instruction(i, item_cache);
        let smt_operands = build_smt_operands(&operands);
        let smt_operands_ctor = build_smt_operands_ctor(&operands);
        let operand_params = if smt_operands.is_empty() {
            "()".to_string()
        } else {
            format!("({})", smt_operands.join(" "))
        };
        let execute_params = if smt_operands.is_empty() {
            "((st TMDLState))".to_string()
        } else {
            format!("((st TMDLState) {})", smt_operands.join(" "))
        };
        let smt_encoding = build_smt_encoding(item_cache, i, &operands);
        let smt_behavior = build_smt_behavior(item_cache, i, &operands);

        let operand_list = operands
            .iter()
            .map(|(k, _v)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");

        writeln!(
            output,
            "\n(define-fun encode_{name} {operand_params} (_ BitVec 32)\n  {smt_encoding})\n\n(define-fun execute_{name} {execute_params} TMDLState\n  {smt_behavior})"
        )?;

        if smt_operands_ctor.is_empty() {
            instruction_variants.push(format!("({uppercase_name})"));
        } else {
            instruction_variants.push(format!("({uppercase_name} {smt_operands_ctor})"));
        }

        if operand_list.is_empty() {
            encode_arms.push(format!("(({uppercase_name}) (encode_{name}))"));
            execute_arms.push(format!("(({uppercase_name}) (execute_{name} state))"));
        } else {
            encode_arms.push(format!(
                "(({uppercase_name} {operand_list}) (encode_{name} {operand_list}))"
            ));
            execute_arms.push(format!(
                "(({uppercase_name} {operand_list}) (execute_{name} state {operand_list}))"
            ));
        }
    }

    writeln!(
        output,
        "\n(declare-datatypes () ((TMDLInstr {})))",
        instruction_variants.join(" ")
    )?;

    writeln!(
        output,
        "\n(define-fun encode_{dialect} ((instr TMDLInstr)) (_ BitVec 32)\n  (match instr\n    {encode_arms}))",
        encode_arms = encode_arms.join("\n    ")
    )?;

    writeln!(
        output,
        "\n(define-fun execute_{dialect} ((state TMDLState) (instr TMDLInstr)) TMDLState\n  (match instr\n    {execute_arms}))",
        execute_arms = execute_arms.join("\n    ")
    )?;

    Ok(())
}

fn build_smt_operands(operands: &[(String, ast::Type)]) -> Vec<String> {
    operands
        .iter()
        .map(|(name, ty)| format!("({} {})", name.to_lowercase(), smt_ty_of(ty)))
        .collect::<Vec<_>>()
}

fn build_smt_operands_ctor(operands: &[(String, ast::Type)]) -> String {
    operands
        .iter()
        .map(|(name, ty)| format!("({} {})", name.to_lowercase(), smt_ty_of(ty)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn smt_ty_of(ty: &ast::Type) -> String {
    match ty {
        ast::Type::Struct(_) => format!("(_ BitVec {REG_INDEX_WIDTH})"),
        ast::Type::Bits(_) | ast::Type::Integer => format!("(_ BitVec {REG_VALUE_WIDTH})"),
        ast::Type::String => "String".to_string(),
    }
}

fn build_smt_encoding<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
    operands: &[(String, ast::Type)],
) -> String {
    let operands = operands.iter().cloned().collect::<HashMap<_, _>>();

    let params = resolve_params_for_instruction(instruction, item_cache);

    let encoding_arms = get_encoding_arms(instruction, item_cache);

    let mut pieces: Vec<(u16, String)> = Vec::new();
    for arm in &encoding_arms {
        let start = arm.start;
        let end = arm.end.unwrap_or(start);
        let width: u16 = end - start + 1;
        let high_bit = end;

        let piece = match &arm.value {
            ast::Expr::Lit(ast::Lit::Int(li)) => render_lit_bitvec(width, li),
            ast::Expr::Ident(id) => {
                let name = &id.name;
                if let Some(ty) = operands.get(name) {
                    let vname = name.to_lowercase();
                    match ty {
                        ast::Type::Struct(_) => cast_bv(&vname, REG_INDEX_WIDTH, width),
                        ast::Type::Bits(_) | ast::Type::Integer => {
                            cast_bv(&vname, REG_VALUE_WIDTH, width)
                        }
                        ast::Type::String => zero_bv(width),
                    }
                } else if let Some((pty, pval)) = params.get(name) {
                    match pval {
                        Some(ast::Expr::Lit(ast::Lit::Int(li))) => render_lit_bitvec(width, li),
                        _ => match pty {
                            ast::Type::Bits(_) | ast::Type::Integer => zero_bv(width),
                            _ => zero_bv(width),
                        },
                    }
                } else {
                    zero_bv(width)
                }
            }
            ast::Expr::Slice(s) => {
                let base_str = match &*s.base {
                    ast::Expr::Ident(id) => id.name.to_lowercase(),
                    _ => "(_ bv0 64)".to_string(),
                };
                format!("((_ extract {} {}) {})", s.end, s.start, base_str)
            }
            ast::Expr::IndexAccess(s) => {
                let base_str = match &*s.base {
                    ast::Expr::Ident(id) => id.name.to_lowercase(),
                    _ => "(_ bv0 64)".to_string(),
                };
                format!("((_ extract {} {}) {})", s.index, s.index, base_str)
            }
            _ => zero_bv(width),
        };

        pieces.push((high_bit, piece));
    }

    pieces.sort_by(|a, b| b.0.cmp(&a.0));

    let mut iter = pieces.iter().map(|(_, p)| p.clone());
    let mut out = iter.next().unwrap_or_else(|| "(_ bv0 32)".to_string());
    for p in iter {
        out = format!("(concat {} {})", out, p);
    }

    out
}

fn build_smt_behavior<'a>(
    _item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
    operands: &[(String, ast::Type)],
) -> String {
    let operands = operands.iter().cloned().collect::<HashMap<_, _>>();

    fn eval_expr(e: &ast::Expr, operands: &HashMap<String, ast::Type>) -> String {
        match e {
            ast::Expr::Lit(ast::Lit::Int(li)) => render_bv64_literal(li),
            ast::Expr::Lit(ast::Lit::Str(ls)) => format!("\"{}\"", ls.value()),
            ast::Expr::Ident(id) => {
                let name = id.name.to_lowercase();
                if let Some(ty) = operands.get(&id.name) {
                    match ty {
                        ast::Type::Struct(rc) => {
                            format!("(read_{} st {})", rc.to_lowercase(), name)
                        }
                        _ => name,
                    }
                } else {
                    name
                }
            }
            ast::Expr::Binary(b) => {
                let lhs = eval_expr(&b.lhs, operands);
                let rhs = eval_expr(&b.rhs, operands);
                let op = match b.op {
                    ast::BinOp::Add => "bvadd",
                    ast::BinOp::Sub => "bvsub",
                    ast::BinOp::Mul => "bvmul",
                    ast::BinOp::Div => "bvudiv",
                    ast::BinOp::BitwiseAnd => "bvand",
                    ast::BinOp::BitwiseOr => "bvor",
                    ast::BinOp::BitwiseXor => "bvxor",
                    ast::BinOp::ShiftLeftLogical => "bvshl",
                    ast::BinOp::ShiftRightLogical => "bvlshr",
                    ast::BinOp::ShiftRightArithmetic => "bvashr",
                };
                format!("({} {} {})", op, lhs, rhs)
            }
            ast::Expr::Slice(s) => eval_expr(&s.base, operands),
            ast::Expr::IndexAccess(s) => eval_expr(&s.base, operands),
            ast::Expr::Field(f) => {
                if let ast::Expr::Ident(id) = &*f.base {
                    if id.name == "self" {
                        return f.member.to_lowercase();
                    }
                }
                "(_ bv0 64)".to_string()
            }
            ast::Expr::Block(b) => {
                if b.last_expr_return {
                    if let Some(last) = b.stmts.last() {
                        eval_expr(last, operands)
                    } else {
                        "(_ bv0 64)".to_string()
                    }
                } else {
                    "(_ bv0 64)".to_string()
                }
            }
            ast::Expr::Assign(_) => "(_ bv0 64)".to_string(),
            ast::Expr::If(i) => {
                let c = eval_expr(&i.cond, operands);
                let t = eval_expr(&i.then, operands);
                let e = if let Some(e) = &i.else_ {
                    eval_expr(e, operands)
                } else {
                    "(_ bv0 64)".to_string()
                };
                format!("(ite (not (= {} (_ bv0 64))) {} {})", c, t, e)
            }
            ast::Expr::BuiltinFunction(_) | ast::Expr::Call(_) | ast::Expr::Invalid => {
                "(_ bv0 64)".to_string()
            }
        }
    }

    fn compile_to_state(
        e: &ast::Expr,
        operands: &HashMap<String, ast::Type>,
        st_name: &str,
    ) -> String {
        match e {
            ast::Expr::Assign(a) => {
                let dst_name = a.dest.to_lowercase();
                let rhs = eval_expr(&a.value, operands);
                if a.dest == "pc" {
                    format!("(write_pc {} {})", st_name, rhs)
                } else if let Some(ast::Type::Struct(rc)) = operands.get(&a.dest) {
                    format!(
                        "(write_{} {} {} {})",
                        rc.to_lowercase(),
                        st_name,
                        dst_name,
                        rhs
                    )
                } else {
                    st_name.to_string()
                }
            }
            ast::Expr::Block(b) => {
                let mut current = st_name.to_string();
                for stmt in &b.stmts {
                    match stmt {
                        ast::Expr::Assign(_) | ast::Expr::Block(_) | ast::Expr::If(_) => {
                            let next = compile_to_state(stmt, operands, &current);
                            current = next;
                        }
                        _ => {}
                    }
                }
                current
            }
            ast::Expr::If(i) => {
                let cond = eval_expr(&i.cond, operands);
                let then_state = compile_to_state(&i.then, operands, st_name);
                let else_state = if let Some(e) = &i.else_ {
                    compile_to_state(e, operands, st_name)
                } else {
                    st_name.to_string()
                };
                format!(
                    "(ite (not (= {} (_ bv0 64))) {} {})",
                    cond, then_state, else_state
                )
            }
            _ => st_name.to_string(),
        }
    }

    compile_to_state(&instruction.behavior, &operands, "st")
}

fn get_encoding_arms<'a>(
    instruction: &'a Instruction,
    item_cache: &HashMap<String, &'a Item>,
) -> Vec<ast::EncodingArm> {
    if !instruction.encoding.is_empty() {
        instruction.encoding.clone()
    } else {
        let mut cur = instruction.parent_template.as_ref();
        while let Some(name) = cur {
            if let Some(ast::Item::Template(t)) = item_cache.get(name.as_str()) {
                if !t.encoding.is_empty() {
                    return t.encoding.clone();
                }
                cur = t.parent_template.as_ref();
            } else {
                break;
            }
        }
        Vec::new()
    }
}

fn resolve_params_for_instruction<'a>(
    inst: &'a ast::Instruction,
    cache: &HashMap<String, &'a ast::Item>,
) -> HashMap<String, (ast::Type, Option<ast::Expr>)> {
    let mut result: HashMap<String, (ast::Type, Option<ast::Expr>)> = HashMap::new();
    fn collect_from_template<'a>(
        name: &str,
        cache: &HashMap<String, &'a ast::Item>,
        acc: &mut HashMap<String, (ast::Type, Option<ast::Expr>)>,
    ) {
        if let Some(ast::Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.params {
                acc.entry(k.clone()).or_insert(v.clone());
            }
        }
    }

    if let Some(p) = &inst.parent_template {
        collect_from_template(p, cache, &mut result);
    }

    for (k, v) in &inst.params {
        result.insert(k.clone(), v.clone());
    }

    result
}

fn render_lit_bitvec(width: u16, lit: &ast::LitInt) -> String {
    let value = parse_literal_value(lit);
    format!("(_ bv{} {})", value, width)
}

fn zero_bv(width: u16) -> String {
    format!("(_ bv0 {})", width)
}

fn render_bv64_literal(lit: &ast::LitInt) -> String {
    let value = parse_literal_value(lit);
    format!("(_ bv{} 64)", value)
}

fn parse_literal_value(lit: &ast::LitInt) -> u128 {
    let v = lit.value();
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        u128::from_str_radix(hex, 16).unwrap_or(0)
    } else if let Some(bin) = v.strip_prefix("0b") {
        u128::from_str_radix(bin, 2).unwrap_or(0)
    } else {
        v.parse::<u128>().unwrap_or(0)
    }
}

fn cast_bv(name: &str, from_width: u16, to_width: u16) -> String {
    match from_width.cmp(&to_width) {
        std::cmp::Ordering::Equal => name.to_string(),
        std::cmp::Ordering::Less => {
            format!("((_ zero_extend {}) {})", to_width - from_width, name)
        }
        std::cmp::Ordering::Greater => {
            format!("((_ extract {} 0) {})", to_width - 1, name)
        }
    }
}

const HEADER: &str = "; Automatically generated by TMDL compiler\n(set-logic AUFBV)\n";
