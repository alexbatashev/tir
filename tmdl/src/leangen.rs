use std::collections::HashMap;
use std::io::Write;

use crate::ast::{self, Instruction, Item};
use crate::error::TMDLError;
use crate::utils::resolve_operands_for_instruction;

pub fn generate_lean(
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
        .map(|file| {
            file.items
                .iter()
                .filter_map(|item: &Item| item.as_register_class().cloned())
        })
        .flatten()
        .collect::<Vec<_>>();

    writeln!(output, "structure TMDLState where")?;
    for rc in &reg_classes {
        let name = &rc.name.to_lowercase();
        let regcount = 32;
        writeln!(output, "  {name} : Fin {regcount} → BitVec 64")?;
    }
    writeln!(output, "  pc : BitVec 64")?;

    for rc in &reg_classes {
        let name = &rc.name.to_lowercase();
        writeln!(
            output,
            "\ndef TMDLState.read_{name}(st : TMDLState) (r : Nat) : BitVec 64 :=\n  if h0 : r = 0 then\n    0\n  else if h : r < 32 then\n    st.{name} ⟨r, h⟩\n  else\n    0\n\ndef TMDLState.write_{name}(st : TMDLState) (r : Nat) (val : BitVec 64) : TMDLState :=\n  if h0 : r = 0 then\n    st\n  else if h : r < 32 then\n    {{ st with {name} := fun i => if i = ⟨r, h⟩ then val else st.{name} i }}\n  else\n    st\n"
        )?;
    }

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
        .map(|file| {
            file.items
                .iter()
                .filter_map(|item: &Item| item.as_instruction())
        })
        .flatten()
        .collect::<Vec<_>>();

    let mut instruction_variants = vec![];
    let mut encode_arms = vec![];
    let mut execute_arms = vec![];

    for i in &instructions {
        let name = i.name.to_lowercase();
        let uppercase_name = name.to_uppercase();

        let operands = resolve_operands_for_instruction(i, item_cache);

        let lean_operands = build_lean_operands(item_cache, &operands);
        let lean_encoding = build_lean_encoding(item_cache, i);
        let lean_behavior = build_lean_behavior(item_cache, i);

        let operand_list = operands
            .iter()
            .map(|(k, _v)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");

        writeln!(
            output,
            "\ndef encode_{name} {lean_operands} : BitVec 32 :=\n  {lean_encoding}\n\ndef execute_{name} (st : TMDLState) {lean_operands} : TMDLState :=\n  {lean_behavior}\n"
        )?;

        if lean_operands.is_empty() {
            instruction_variants.push(format!("| {uppercase_name}"));
        } else {
            instruction_variants.push(format!("| {uppercase_name} {lean_operands}"));
        }
        encode_arms.push(format!(
            "| .{uppercase_name} {operand_list} => encode_{name} {operand_list}"
        ));
        execute_arms.push(format!(
            "| .{uppercase_name} {operand_list} => execute_{name} state {operand_list}"
        ));
    }

    let instruction_variants = instruction_variants.join("\n    ");
    let encode_arms = encode_arms.join("\n    ");
    let execute_arms = execute_arms.join("\n    ");
    writeln!(
        output,
        "\ninductive TMDLInstr where\n    {instruction_variants}\n    deriving Repr, BEq\n\ndef encode_{dialect} (instr : TMDLInstr) : BitVec 32 :=\n  match instr with\n  {encode_arms}\n\ndef execute_{dialect} (state : TMDLState) (instr : TMDLInstr) : TMDLState :=\n  match instr with\n  {execute_arms}\n"
    )?;

    generate_structural_decoder(output, dialect, item_cache, &instructions)?;

    Ok(())
}

#[derive(Debug)]
struct InstructionPattern {
    name: String,
    mask: u64,
    expected: u64,
    operand_extracts: Vec<(String, u16, u16, ast::Type)>, // (operand_name, start_bit, end_bit, type)
}

fn analyze_instruction_encoding<'a>(
    instruction: &'a Instruction,
    item_cache: &HashMap<String, &'a Item>,
) -> InstructionPattern {
    let encoding_arms = get_encoding_arms(instruction, item_cache);
    let operands = resolve_operands_for_instruction(instruction, item_cache);
    let operands_map: HashMap<_, _> = operands.iter().cloned().collect();
    let params = resolve_params_for_instruction(instruction, item_cache);

    let mut mask: u64 = 0;
    let mut expected: u64 = 0;
    let mut operand_extracts = Vec::new();

    for arm in &encoding_arms {
        let start = arm.start;
        let end = arm.end.unwrap_or(start);

        match &arm.value {
            ast::Expr::Lit(ast::Lit::Int(li)) => {
                let value = parse_literal_value(li);
                for bit in start..=end {
                    mask |= 1u64 << bit;
                    if (value >> (bit - start)) & 1 == 1 {
                        expected |= 1u64 << bit;
                    }
                }
            }
            ast::Expr::Ident(id) => {
                let name = &id.name;
                if let Some(ty) = operands_map.get(name) {
                    operand_extracts.push((name.to_lowercase(), start, end, ty.clone()));
                } else if let Some((_, Some(ast::Expr::Lit(ast::Lit::Int(li))))) = params.get(name)
                {
                    let value = parse_literal_value(li);
                    for bit in start..=end {
                        mask |= 1u64 << bit;
                        if (value >> (bit - start)) & 1 == 1 {
                            expected |= 1u64 << bit;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    InstructionPattern {
        name: instruction.name.clone(),
        mask,
        expected,
        operand_extracts,
    }
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
                acc.insert(k.clone(), v.clone());
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

fn parse_literal_value(lit: &ast::LitInt) -> u64 {
    let v = lit.value();
    if v.starts_with("0b") {
        u64::from_str_radix(&v[2..], 2).unwrap_or(0)
    } else if v.starts_with("0x") || v.starts_with("0X") {
        u64::from_str_radix(&v[2..], 16).unwrap_or(0)
    } else {
        v.parse::<u64>().unwrap_or(0)
    }
}

fn generate_structural_decoder(
    output: &mut Box<dyn Write>,
    dialect: &str,
    item_cache: &HashMap<String, &Item>,
    instructions: &[&Instruction],
) -> Result<(), TMDLError> {
    writeln!(output, "\n-- Bit field extraction helper")?;
    writeln!(
        output,
        "def extractBits (w : BitVec 32) (start : Nat) (len : Nat) : Nat :=\n  let bits := w.toNat\n  let shifted := Nat.shiftRight bits start\n  let mask := (Nat.shiftLeft 1 len) - 1\n  Nat.land shifted mask\n"
    )?;

    let patterns: Vec<InstructionPattern> = instructions
        .iter()
        .map(|instr| analyze_instruction_encoding(instr, item_cache))
        .collect();

    writeln!(output, "-- Structural decoder using mask-and-match")?;
    writeln!(
        output,
        "def decode_{dialect} (w : BitVec 32) : Option TMDLInstr :="
    )?;
    writeln!(output, "  let bits := w.toNat")?;

    let mut first = true;
    for pattern in &patterns {
        let uppercase_name = pattern.name.to_uppercase();

        let mut operand_vals = Vec::new();
        for (_op_name, start, end, ty) in &pattern.operand_extracts {
            let width = end - start + 1;
            let extracted = format!("(extractBits w {} {})", start, width);
            let converted = match ty {
                ast::Type::Struct(_) => extracted.clone(),
                ast::Type::Bits(w) => format!("(BitVec.ofNat {} {})", w, extracted),
                ast::Type::Integer => format!("(Int.ofNat {})", extracted),
                ast::Type::String => format!("(toString {})", extracted),
            };
            operand_vals.push(converted);
        }

        let result = if operand_vals.is_empty() {
            format!("some TMDLInstr.{}", uppercase_name)
        } else {
            format!(
                "some (TMDLInstr.{} {})",
                uppercase_name,
                operand_vals.join(" ")
            )
        };

        let prefix = if first {
            first = false;
            "  if".to_string()
        } else {
            "  else if".to_string()
        };

        writeln!(
            output,
            "{prefix} h : Nat.land bits {mask} = {expected} then",
            prefix = prefix,
            mask = pattern.mask,
            expected = pattern.expected
        )?;
        writeln!(output, "    {result}")?;
    }

    writeln!(output, "  else")?;
    writeln!(output, "    none")?;
    writeln!(output)?;
    Ok(())
}

/// For a list of operands returns a string of function operands in Lean format. Examples:
/// (rd rs1 rs2 : Nat)
/// (rd rs1 : Nat) (imm : BitVec 12)
fn build_lean_operands<'cache>(
    item_cache: &HashMap<String, &'cache Item>,
    operands: &Vec<(String, ast::Type)>,
) -> String {
    let _ = item_cache;

    fn lean_ty_of(t: &ast::Type) -> String {
        match t {
            ast::Type::Struct(_) => "Nat".to_string(),
            ast::Type::Bits(w) => format!("BitVec {}", w),
            ast::Type::Integer => "Int".to_string(),
            ast::Type::String => "String".to_string(),
        }
    }

    if operands.is_empty() {
        return String::new();
    }

    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (name, ty) in operands.iter() {
        let lname = name.to_lowercase();
        let lty = lean_ty_of(ty);
        if let Some((cur_ty, names)) = groups.last_mut() {
            if *cur_ty == lty {
                names.push(lname);
                continue;
            }
        }
        groups.push((lty, vec![lname]));
    }

    let mut parts: Vec<String> = Vec::new();
    for (ty, names) in groups {
        parts.push(format!("({} : {})", names.join(" "), ty));
    }

    parts.join(" ")
}

/// Builds an encoding statement from encoding description.
fn build_lean_encoding<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();

    let params = resolve_params_for_instruction(instruction, item_cache);

    fn render_lit_bitvec(width: u16, lit: &ast::LitInt) -> String {
        let v = lit.value();
        if v.starts_with("0b") || v.starts_with("0x") || v.starts_with("0X") {
            format!("({} : BitVec {})", v, width)
        } else {
            format!("(BitVec.ofNat {} {})", width, v)
        }
    }

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
                        ast::Type::Struct(_) => format!("(BitVec.ofNat {} {})", width, vname),
                        ast::Type::Bits(_w) => format!("({})", vname),
                        ast::Type::Integer => format!("(BitVec.ofNat {} {})", width, vname),
                        ast::Type::String => format!("(BitVec.ofNat {} 0)", width),
                    }
                } else if let Some((pty, pval)) = params.get(name) {
                    match pval {
                        Some(ast::Expr::Lit(ast::Lit::Int(li))) => render_lit_bitvec(width, li),
                        _ => match pty {
                            ast::Type::Bits(_) | ast::Type::Integer => {
                                format!("(BitVec.ofNat {} 0)", width)
                            }
                            _ => format!("(BitVec.ofNat {} 0)", width),
                        },
                    }
                } else {
                    format!("(BitVec.ofNat {} 0)", width)
                }
            }
            ast::Expr::Slice(s) => {
                let base_str = match &*s.base {
                    ast::Expr::Ident(id) => id.name.to_lowercase(),
                    _ => "0".to_string(),
                };
                format!("(BitVec.slice {} {} {})", base_str, s.start, s.end)
            }
            ast::Expr::IndexAccess(s) => {
                let base_str = match &*s.base {
                    ast::Expr::Ident(id) => id.name.to_lowercase(),
                    _ => "0".to_string(),
                };
                format!("(BitVec.slice {} {} {})", base_str, s.index, s.index)
            }
            _ => format!("(BitVec.ofNat {} 0)", width),
        };

        pieces.push((high_bit, piece));
    }

    pieces.sort_by(|a, b| b.0.cmp(&a.0));

    let mut out = String::new();
    for (idx, (_hb, p)) in pieces.iter().enumerate() {
        if idx + 1 < pieces.len() {
            out.push_str(&format!("{} ++\n  ", p));
        } else {
            out.push_str(p);
        }
    }

    out
}

/// Builds a function body for TMDL behavior region.
fn build_lean_behavior<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();

    fn eval_expr(e: &ast::Expr, operands: &HashMap<String, ast::Type>) -> String {
        match e {
            ast::Expr::Lit(ast::Lit::Int(li)) => li.value().to_string(),
            ast::Expr::Lit(ast::Lit::Str(ls)) => format!("\"{}\"", ls.value()),
            ast::Expr::Ident(id) => {
                let name = id.name.to_lowercase();
                if let Some(ty) = operands.get(&id.name) {
                    match ty {
                        ast::Type::Struct(rc) => {
                            format!("(st.read_{} {})", rc.to_lowercase(), name)
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
                    ast::BinOp::Add => "+",
                    ast::BinOp::Sub => "-",
                    ast::BinOp::Mul => "*",
                    ast::BinOp::Div => "/",
                    ast::BinOp::BitwiseAnd => "&&&",
                    ast::BinOp::BitwiseOr => "|||",
                    ast::BinOp::BitwiseXor => "^^^",
                    ast::BinOp::ShiftLeftLogical => "<<<",
                    ast::BinOp::ShiftRightLogical => ">>>",
                    ast::BinOp::ShiftRightArithmetic => ">>>",
                };
                format!("({} {} {})", lhs, op, rhs)
            }
            ast::Expr::Slice(s) => eval_expr(&s.base, operands),
            ast::Expr::IndexAccess(s) => eval_expr(&s.base, operands),
            ast::Expr::Field(f) => {
                if let ast::Expr::Ident(id) = &*f.base {
                    if id.name == "self" {
                        return f.member.to_lowercase();
                    }
                }
                "0".to_string()
            }
            ast::Expr::Block(b) => {
                if b.last_expr_return {
                    if let Some(last) = b.stmts.last() {
                        eval_expr(last, operands)
                    } else {
                        "0".to_string()
                    }
                } else {
                    "0".to_string()
                }
            }
            ast::Expr::Assign(_) => "0".to_string(),
            ast::Expr::If(i) => {
                let c = eval_expr(&i.cond, operands);
                let t = eval_expr(&i.then, operands);
                let e = if let Some(e) = &i.else_ {
                    eval_expr(e, operands)
                } else {
                    "0".to_string()
                };
                format!("(if {} then {} else {})", c, t, e)
            }
            ast::Expr::Call(_) | ast::Expr::Invalid => "0".to_string(),
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
                if let Some(ast::Type::Struct(rc)) = operands.get(&a.dest) {
                    format!(
                        "({}.write_{} {} {})",
                        st_name,
                        rc.to_lowercase(),
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
                format!("(if {} then {} else {})", cond, then_state, else_state)
            }
            _ => st_name.to_string(),
        }
    }

    compile_to_state(&instruction.behavior, &operands, "st")
}

const HEADER: &str = "-- Automatically generated by TMDL compiler
import Std.Data.BitVec
open Std
open Nat
open BitVec

-- Basic bitvector operations and notations used by the generated semantics
def bvAdd {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (a.toNat + b.toNat)
def bvSub {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (a.toNat - b.toNat)
def bvAnd {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (Nat.land (a.toNat) (b.toNat))
def bvOr  {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (Nat.lor  (a.toNat) (b.toNat))
def bvXor {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (Nat.xor (a.toNat) (b.toNat))
def bvShl {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (Nat.shiftLeft (a.toNat) b.toNat)
def bvShr {n} (a b : BitVec n) : BitVec n := BitVec.ofNat n (Nat.shiftRight (a.toNat) b.toNat)
def bvConcat {n m} (a : BitVec n) (b : BitVec m) : BitVec (n + m) := BitVec.ofNat _ ((Nat.shiftLeft (a.toNat) m) + b.toNat)

notation:50 lhs \\\" + \\\" rhs => bvAdd lhs rhs
notation:50 lhs \\\" - \\\" rhs => bvSub lhs rhs
notation:40 lhs \\\" ^^^ \\\" rhs => bvXor lhs rhs
notation:40 lhs \\\" ||| \\\" rhs => bvOr lhs rhs
notation:40 lhs \\\" &&& \\\" rhs => bvAnd lhs rhs
notation:35 lhs \\\" <<< \\\" rhs => bvShl lhs rhs
notation:35 lhs \\\" >>> \\\" rhs => bvShr lhs rhs
notation:60 lhs \\\" ++ \\\" rhs => bvConcat lhs rhs
";
