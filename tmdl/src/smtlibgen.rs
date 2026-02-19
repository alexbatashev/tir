use std::collections::HashMap;
use std::io::Write;

use crate::Type;
use crate::ast;
use crate::error::TMDLError;
use crate::sem_expr_conv::{SymbolInfo, convert_to_sem_expr};
use crate::sem_expr_state;
use crate::utils::{
    get_encoding_arms, resolve_operands_for_instruction, resolve_params_for_instruction,
};
use tir::sem_expr::smtlib as sem_smtlib;

const REG_INDEX_WIDTH: u16 = 5;
const REG_VALUE_WIDTH: u16 = 64;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn generate_smtlib<'a>(
    dialect: &str,
    files: &'a [ast::File],
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    writeln!(output, "{}", HEADER)?;
    build_state(files, &mut output)?;
    build_instructions(dialect, item_cache, files, &mut output)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// State (register file) declaration
// ---------------------------------------------------------------------------

fn build_state(files: &[ast::File], output: &mut Box<dyn Write>) -> Result<(), TMDLError> {
    let reg_class_names = files
        .iter()
        .flat_map(|f| f.register_classes())
        .map(|rc| rc.name.to_lowercase())
        .collect::<Vec<_>>();

    let mut fields = reg_class_names
        .iter()
        .map(|name| {
            format!(
            "({} (Array (_ BitVec {}) (_ BitVec {})))",
            name, REG_INDEX_WIDTH, REG_VALUE_WIDTH
            )
        })
        .collect::<Vec<_>>();
    fields.push(format!("(pc (_ BitVec {}))", REG_VALUE_WIDTH));

    writeln!(
        output,
        "(declare-datatypes () ((TMDLState (mk-TMDLState {}))))",
        fields.join(" ")
    )?;

    for name in &reg_class_names {
        writeln!(
            output,
            "\n(define-fun read_{name} ((st TMDLState) (r (_ BitVec {idx_width}))) (_ BitVec {val_width})\n  (ite (= r (_ bv0 {idx_width}))\n    (_ bv0 {val_width})\n    (select ({name} st) r)))",
            idx_width = REG_INDEX_WIDTH,
            val_width = REG_VALUE_WIDTH
        )?;

        let mut fields = Vec::new();
        for n2 in &reg_class_names {
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

    let mut fields = reg_class_names
        .iter()
        .map(|name| format!("({} st)", name))
        .collect::<Vec<_>>();
    fields.push("val".to_string());
    writeln!(
        output,
        "\n(define-fun write_pc ((st TMDLState) (val (_ BitVec {val_width}))) TMDLState\n  (mk-TMDLState {fields}))",
        val_width = REG_VALUE_WIDTH,
        fields = fields.join(" ")
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Instruction encoding and execution
// ---------------------------------------------------------------------------

fn build_instructions<'a>(
    dialect: &str,
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    files: &'a [ast::File],
    output: &mut Box<dyn Write>,
) -> Result<(), TMDLError> {
    let mut instruction_variants = vec![];
    let mut encode_arms = vec![];
    let mut execute_arms = vec![];

    for i in files.iter().flat_map(|f| f.instructions()) {
        let name = i.name.to_lowercase();
        let uppercase_name = name.to_uppercase();

        let operands = resolve_operands_for_instruction(i, item_cache);
        let smt_operands = build_smt_operands(&operands);
        let smt_operands_joined = smt_operands.join(" ");
        let operand_params = if smt_operands_joined.is_empty() {
            "()".to_string()
        } else {
            format!("({smt_operands_joined})")
        };
        let execute_params = if smt_operands_joined.is_empty() {
            "((st TMDLState))".to_string()
        } else {
            format!("((st TMDLState) {smt_operands_joined})")
        };
        let smt_encoding = build_smt_encoding(item_cache, i, &operands);
        let smt_behavior = build_smt_behavior(item_cache, i, &operands);

        let operand_names = operands
            .iter()
            .map(|(k, _v)| k.to_lowercase())
            .collect::<Vec<_>>();
        let operand_list = operand_names.join(" ");

        writeln!(
            output,
            "\n(define-fun encode_{name} {operand_params} (_ BitVec 32)\n  {smt_encoding})\n\n(define-fun execute_{name} {execute_params} TMDLState\n  {smt_behavior})"
        )?;

        if smt_operands_joined.is_empty() {
            instruction_variants.push(format!("({uppercase_name})"));
        } else {
            instruction_variants.push(format!("({uppercase_name} {smt_operands_joined})"));
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

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

fn build_smt_operands(operands: &[(String, Type)]) -> Vec<String> {
    operands
        .iter()
        .map(|(name, ty)| format!("({} {})", name.to_lowercase(), smt_ty_of(ty)))
        .collect()
}

fn smt_ty_of(ty: &Type) -> String {
    match ty {
        Type::Struct(_) => format!("(_ BitVec {REG_INDEX_WIDTH})"),
        Type::Bits(_) | Type::Integer => format!("(_ BitVec {REG_VALUE_WIDTH})"),
        Type::String => "String".to_string(),
        _ => unreachable!("HM type vars should not appear as operand types"),
    }
}

fn build_smt_encoding<'a>(
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    instruction: &'a ast::Instruction,
    operands: &[(String, Type)],
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
                        Type::Struct(_) => cast_bv(&vname, REG_INDEX_WIDTH, width),
                        Type::Bits(_) | Type::Integer => cast_bv(&vname, REG_VALUE_WIDTH, width),
                        Type::String => zero_bv(width),
                        _ => unreachable!("HM type vars should not appear as operand types"),
                    }
                } else if let Some((pty, pval)) = params.get(name) {
                    match pval {
                        Some(ast::Expr::Lit(ast::Lit::Int(li))) => render_lit_bitvec(width, li),
                        _ => match pty {
                            Type::Bits(_) | Type::Integer => zero_bv(width),
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

    let mut iter = pieces.into_iter().map(|(_, piece)| piece);
    iter.next()
        .map(|first| iter.fold(first, |acc, piece| format!("(concat {} {})", acc, piece)))
        .unwrap_or_else(|| "(_ bv0 32)".to_string())
}

// ---------------------------------------------------------------------------
// Behavior (execution semantics)
// ---------------------------------------------------------------------------

struct SmtSymbolResolver<'a> {
    symbols: &'a HashMap<u32, SymbolInfo>,
    operands: &'a HashMap<String, Type>,
    state_name: &'a str,
}

impl sem_smtlib::SymbolResolver for SmtSymbolResolver<'_> {
    fn resolve(&self, symbol_id: u32) -> Result<String, String> {
        let symbol = self
            .symbols
            .get(&symbol_id)
            .ok_or_else(|| format!("Unknown symbol id {}", symbol_id))?;

        match symbol {
            SymbolInfo::Register { class, number } => Ok(format!(
                "(read_{} {} (_ bv{} {}))",
                class.to_lowercase(),
                self.state_name,
                number,
                REG_INDEX_WIDTH
            )),
            SymbolInfo::Variable { name } => {
                if let Some(Type::Struct(rc)) = self.operands.get(name) {
                    Ok(format!(
                        "(read_{} {} {})",
                        rc.to_lowercase(),
                        self.state_name,
                        name.to_lowercase()
                    ))
                } else {
                    Ok(name.to_lowercase())
                }
            }
        }
    }
}

fn build_smt_behavior<'a>(
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    instruction: &'a ast::Instruction,
    operands: &[(String, Type)],
) -> String {
    let operands = operands.iter().cloned().collect::<HashMap<_, _>>();
    let numeric_params: HashMap<_, _> = resolve_params_for_instruction(instruction, item_cache)
        .into_iter()
        .filter_map(|(name, (_ty, val))| match val {
            Some(ast::Expr::Lit(ast::Lit::Int(li))) => {
                Some((name, parse_literal_value_u128(&li) as i64))
            }
            _ => None,
        })
        .collect();

    fn try_emit_sem_expr(
        e: &ast::Expr,
        operands: &HashMap<String, Type>,
        numeric_params: &HashMap<String, i64>,
        state_name: &str,
    ) -> Option<String> {
        let converted = convert_to_sem_expr(e, numeric_params.clone()).ok()?;
        let resolver = SmtSymbolResolver {
            symbols: &converted.symbols,
            operands,
            state_name,
        };
        let mut out = Vec::new();
        sem_smtlib::emit(&converted.expr, &mut out, &resolver).ok()?;
        String::from_utf8(out).ok()
    }

    fn eval_expr_legacy(e: &ast::Expr, operands: &HashMap<String, Type>) -> String {
        match e {
            ast::Expr::Lit(ast::Lit::Int(li)) => render_bv64_literal(li),
            ast::Expr::Lit(ast::Lit::Str(ls)) => format!("\"{}\"", ls.value()),
            ast::Expr::Ident(id) => {
                let name = id.name.to_lowercase();
                if let Some(ty) = operands.get(&id.name) {
                    match ty {
                        Type::Struct(rc) => {
                            format!("(read_{} st {})", rc.to_lowercase(), name)
                        }
                        _ => name,
                    }
                } else {
                    name
                }
            }
            ast::Expr::Binary(b) => {
                let lhs = eval_expr_legacy(&b.lhs, operands);
                let rhs = eval_expr_legacy(&b.rhs, operands);
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
            ast::Expr::Slice(s) => eval_expr_legacy(&s.base, operands),
            ast::Expr::IndexAccess(s) => eval_expr_legacy(&s.base, operands),
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
                        eval_expr_legacy(last, operands)
                    } else {
                        "(_ bv0 64)".to_string()
                    }
                } else {
                    "(_ bv0 64)".to_string()
                }
            }
            ast::Expr::Assign(_) => "(_ bv0 64)".to_string(),
            ast::Expr::If(i) => {
                let c = eval_expr_legacy(&i.cond, operands);
                let t = eval_expr_legacy(&i.then, operands);
                let e = if let Some(e) = &i.else_ {
                    eval_expr_legacy(e, operands)
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

    fn eval_expr(
        e: &ast::Expr,
        operands: &HashMap<String, Type>,
        numeric_params: &HashMap<String, i64>,
    ) -> String {
        if let Some(rendered) = try_emit_sem_expr(e, operands, numeric_params, "st") {
            rendered
        } else {
            eval_expr_legacy(e, operands)
        }
    }

    let emit_expr = |e: &ast::Expr| eval_expr(e, &operands, &numeric_params);
    let emit_assign = |a: &ast::Assign, st_name: &str| {
        let rhs = emit_expr(&a.value);
        if a.dest == "pc" {
            Some(format!("(write_pc {} {})", st_name, rhs))
        } else if let Some(Type::Struct(rc)) = operands.get(&a.dest) {
            Some(format!(
                "(write_{} {} {} {})",
                rc.to_lowercase(),
                st_name,
                a.dest.to_lowercase(),
                rhs
            ))
        } else {
            None
        }
    };
    let emit_if = |cond: &str, then_state: &str, else_state: &str| {
        format!(
            "(ite (not (= {} (_ bv0 64))) {} {})",
            cond, then_state, else_state
        )
    };
    sem_expr_state::compile_to_state(
        &instruction.behavior,
        "st",
        &emit_expr,
        &emit_assign,
        &emit_if,
    )
}

// ---------------------------------------------------------------------------
// Bitvector rendering helpers
// ---------------------------------------------------------------------------

fn render_lit_bitvec(width: u16, lit: &ast::LitInt) -> String {
    let value = parse_literal_value_u128(lit);
    format!("(_ bv{} {})", value, width)
}

fn zero_bv(width: u16) -> String {
    format!("(_ bv0 {})", width)
}

fn render_bv64_literal(lit: &ast::LitInt) -> String {
    let value = parse_literal_value_u128(lit);
    format!("(_ bv{} 64)", value)
}

/// SMT-lib needs the full u128 range for large bitvector literals.
fn parse_literal_value_u128(lit: &ast::LitInt) -> u128 {
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
