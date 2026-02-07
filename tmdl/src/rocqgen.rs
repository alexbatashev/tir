use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::{self, Instruction, Item};
use crate::error::TMDLError;
use crate::sem_expr_conv::{SymbolInfo, convert_to_sem_expr};
use crate::utils::resolve_operands_for_instruction;
use tir::sem_expr::rocq as sem_rocq;

struct RocqSymbolResolver<'a> {
    symbols: &'a HashMap<u32, SymbolInfo>,
    operands: &'a HashMap<String, ast::Type>,
    state_name: &'a str,
}

impl sem_rocq::SymbolResolver for RocqSymbolResolver<'_> {
    fn resolve(&self, symbol_id: u32) -> Result<String, String> {
        let symbol = self
            .symbols
            .get(&symbol_id)
            .ok_or_else(|| format!("Unknown symbol id {}", symbol_id))?;

        match symbol {
            SymbolInfo::Register { class, number } => Ok(format!(
                "(read_{} {} {})",
                class.to_lowercase(),
                self.state_name,
                number
            )),
            SymbolInfo::Variable { name } => {
                if let Some(ast::Type::Struct(rc)) = self.operands.get(name) {
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

pub fn generate_rocq(
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

    // Record with simple nat-indexed register files; mirrors Lean semantics
    writeln!(output, "Record TMDLState := {{")?;
    for (i, rc) in reg_classes.iter().enumerate() {
        let name = rc.name.to_lowercase();
        let sep = if i == reg_classes.len() { "" } else { ";" };
        writeln!(output, "  {} : nat -> tmdl_word 64{}", name, sep)?;
    }
    let sep = if reg_classes.is_empty() { "" } else { ";" };
    writeln!(output, "  pc : tmdl_word 64{}", sep)?;
    writeln!(output, "}}.")?;

    for rc in &reg_classes {
        let name = rc.name.to_lowercase();
        writeln!(
            output,
            "\nDefinition read_{n} (st: TMDLState) (r : nat) : tmdl_word 64 :=\n  if Nat.eqb r 0 then tmdl_word_zero 64\n  else if Nat.ltb r 32 then st.({n}) r else tmdl_word_zero 64.\n",
            n = name
        )?;

        // Build a record update that keeps all other fields intact
        let mut fields: Vec<String> = Vec::new();
        for rc2 in &reg_classes {
            let n2 = rc2.name.to_lowercase();
            if n2 == name {
                fields.push(format!(
                    "{} := fun i => if Nat.eqb i r then val else st.({}) i",
                    n2, n2
                ));
            } else {
                fields.push(format!("{} := st.({})", n2, n2));
            }
        }
        fields.push("pc := st.(pc)".to_string());
        writeln!(
            output,
            "Definition write_{n}(st : TMDLState) (r : nat) (val : tmdl_word 64) : TMDLState :=\n  if Nat.eqb r 0 then st\n  else if Nat.ltb r 32 then\n    {{| {fields} |}}\n  else st.\n",
            n = name,
            fields = fields.join("; ")
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

        let coq_operands = build_coq_operands(item_cache, &operands);
        let coq_operands_ctor = build_coq_operands_ctor(item_cache, &operands);
        let coq_encoding = build_coq_encoding(item_cache, i);
        let coq_behavior = build_coq_behavior(item_cache, i);

        let operand_list = operands
            .iter()
            .map(|(k, _v)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");

        writeln!(
            output,
            "\nDefinition encode_{name} {coq_operands} : tmdl_word 32 :=\n  {coq_encoding}.\n\nDefinition execute_{name} (st: TMDLState) {coq_operands} : TMDLState :=\n  {coq_behavior}.\n"
        )?;

        instruction_variants.push(format!(
            "| {uppercase_name} : {coq_operands_ctor} TMDLInstr"
        ));
        encode_arms.push(format!(
            "| {uppercase_name} {operand_list} => encode_{name} {operand_list}"
        ));
        execute_arms.push(format!(
            "| {uppercase_name} {operand_list} => execute_{name} state {operand_list}"
        ));
    }

    let instruction_variants = instruction_variants.join("\n  ");
    let encode_arms = encode_arms.join("\n    ");
    let execute_arms = execute_arms.join("\n    ");
    writeln!(
        output,
        "\nInductive TMDLInstr : Type :=\n  {instruction_variants}.\n\nDefinition encode_{dialect} (instr : TMDLInstr) : tmdl_word 32 :=\n  match instr with\n    {encode_arms}\n  end.\n\nDefinition execute_{dialect} (state : TMDLState) (instr : TMDLInstr) : TMDLState :=\n  match instr with\n    {execute_arms}\n  end.\n"
    )?;

    // ---------------------------------------------------------------------
    // Structural decoder: match fixed encoding bits for each instruction
    // and extract variable operand fields.
    // ---------------------------------------------------------------------

    generate_structural_decoder(output, dialect, item_cache, &instructions)?;

    Ok(())
}

#[derive(Debug)]
struct InstructionPattern {
    name: String,
    // Mask with 1s for fixed bits, 0s for variable bits
    mask: u64,
    // Expected value (fixed bits in their positions, 0s elsewhere)
    expected: u64,
    // Operand extraction positions with types
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
                // Fixed literal - add to mask and expected value
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
                // Check if it's an operand or a fixed parameter
                if let Some(ty) = operands_map.get(name) {
                    // Variable operand - needs extraction, don't add to mask
                    operand_extracts.push((name.to_lowercase(), start, end, ty.clone()));
                } else if let Some((_, Some(ast::Expr::Lit(ast::Lit::Int(li))))) = params.get(name)
                {
                    // Fixed parameter value - add to mask and expected value
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
    writeln!(output, "\n(* Bit field extraction helper *)")?;
    writeln!(
        output,
        "Definition extract_bits (w : tmdl_word 32) (start : nat) (len : nat) : Z :=\n  Z.land (Z.shiftr (tmdl_word_val w) (Z.of_nat start)) (Z.sub (Z.shiftl 1 (Z.of_nat len)) 1).\n"
    )?;

    // Analyze each instruction's encoding
    let patterns: Vec<InstructionPattern> = instructions
        .iter()
        .map(|instr| analyze_instruction_encoding(instr, item_cache))
        .collect();

    // Generate decoder function using mask-and-match approach
    writeln!(output, "(* Structural decoder using mask-and-match *)")?;
    writeln!(
        output,
        "Definition decode_{} (w : tmdl_word 32) : option TMDLInstr :=",
        dialect
    )?;
    writeln!(output, "  let bits := tmdl_word_val w in")?;

    // Generate flat if-then-else chain for each instruction
    let mut first = true;
    for pattern in &patterns {
        let uppercase_name = pattern.name.to_uppercase();

        // Build operand extractions
        let mut operand_vals = Vec::new();
        for (op_name, start, end, ty) in &pattern.operand_extracts {
            let width = end - start + 1;
            let extracted = format!("(extract_bits w {} {})", start, width);
            // Convert to appropriate type
            let converted = match ty {
                ast::Type::Struct(_) => {
                    // Register index - convert Z to nat
                    format!("(Z.to_nat {})", extracted)
                }
                ast::Type::Bits(_) => {
                    // Already a bitvector, but needs wrapping
                    extracted
                }
                ast::Type::Integer => extracted,
                ast::Type::String => extracted,
            };
            operand_vals.push(converted);
        }

        let result = if operand_vals.is_empty() {
            format!("Some {}", uppercase_name)
        } else {
            format!("Some ({} {})", uppercase_name, operand_vals.join(" "))
        };

        // Generate flat if-then-else: check (bits & mask) == expected
        let prefix = if first {
            first = false;
            "  if"
        } else {
            "  else if"
        };

        writeln!(
            output,
            "{} Z.eqb (Z.land bits {}) {} then",
            prefix, pattern.mask, pattern.expected
        )?;
        writeln!(output, "    {}", result)?;
    }

    // Default case
    writeln!(output, "  else")?;
    writeln!(output, "    None.")?;
    writeln!(output)?;
    Ok(())
}

/// For a list of operands returns a string of function operands in Coq format. Examples:
/// (rd rs1 rs2 : nat)
/// (rd rs1 : nat) (imm : tmdl_word 12)
fn build_coq_operands<'cache>(
    item_cache: &HashMap<String, &'cache Item>,
    operands: &Vec<(String, ast::Type)>,
) -> String {
    // Map a TMDL type to a Coq type string
    fn coq_ty_of(t: &ast::Type) -> String {
        match t {
            ast::Type::Struct(_) => "nat".to_string(),
            ast::Type::Bits(w) => format!("tmdl_word {}", w),
            ast::Type::Integer => "Z".to_string(),
            ast::Type::String => "string".to_string(),
        }
    }

    if operands.is_empty() {
        return String::new();
    }

    // Group consecutive operands with the same Coq type
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (name, ty) in operands.iter() {
        let lname = name.to_lowercase();
        let lty = coq_ty_of(ty);
        if let Some((cur_ty, names)) = groups.last_mut() {
            if *cur_ty == lty {
                names.push(lname);
                continue;
            }
        }
        groups.push((lty, vec![lname]));
    }

    // Render groups as "(a b : Ty) (c : Ty2)"
    let mut parts: Vec<String> = Vec::new();
    for (ty, names) in groups {
        parts.push(format!("({} : {})", names.join(" "), ty));
    }

    parts.join(" ")
}

/// Build a constructor argument list for Coq inductive: "T1 -> T2 ->"
fn build_coq_operands_ctor<'cache>(
    _item_cache: &HashMap<String, &'cache Item>,
    operands: &Vec<(String, ast::Type)>,
) -> String {
    fn coq_ty_of(t: &ast::Type) -> String {
        match t {
            ast::Type::Struct(_) => "nat".to_string(),
            ast::Type::Bits(w) => format!("tmdl_word {}", w),
            ast::Type::Integer => "Z".to_string(),
            ast::Type::String => "string".to_string(),
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for (_name, ty) in operands.iter() {
        parts.push(format!("{} ->", coq_ty_of(ty)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

fn build_coq_encoding<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    // Resolve operands for this instruction
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();

    let params = resolve_params_for_instruction(instruction, item_cache);

    // Helper: render integer literal as tmdl_word of given width
    fn render_lit_bitvec(width: u16, lit: &ast::LitInt) -> String {
        let v = lit.value();
        let decimal_value = parse_literal_value(lit);
        format!("(tmdl_word_of_nat {} {})", width, decimal_value)
    }

    let encoding_arms = get_encoding_arms(instruction, item_cache);

    // Build each arm piece (high-to-low) and concatenate
    let mut pieces: Vec<(u16, String)> = Vec::new();
    for arm in &encoding_arms {
        let start = arm.start;
        let end = arm.end.unwrap_or(start);
        let width: u16 = end - start + 1;
        let high_bit = end; // for sorting later

        let piece = match &arm.value {
            ast::Expr::Lit(ast::Lit::Int(li)) => render_lit_bitvec(width, li),
            ast::Expr::Ident(id) => {
                let name = &id.name;
                if let Some(ty) = operands.get(name) {
                    let vname = name.to_lowercase();
                    match ty {
                        ast::Type::Struct(_) => format!("(tmdl_word_of_nat {} {})", width, vname),
                        ast::Type::Bits(_w) => format!("({})", vname),
                        ast::Type::Integer => format!("(tmdl_word_of_nat {} {})", width, vname),
                        ast::Type::String => format!("(tmdl_word_of_nat {} 0)", width),
                    }
                } else if let Some((pty, pval)) = params.get(name) {
                    match pval {
                        Some(ast::Expr::Lit(ast::Lit::Int(li))) => render_lit_bitvec(width, li),
                        _ => match pty {
                            ast::Type::Bits(_) | ast::Type::Integer => {
                                // Fallback if not a simple literal
                                format!("(tmdl_word_of_nat {} 0)", width)
                            }
                            _ => format!("(tmdl_word_of_nat {} 0)", width),
                        },
                    }
                } else {
                    // Unknown identifier; zero-fill
                    format!("(tmdl_word_of_nat {} 0)", width)
                }
            }
            ast::Expr::Slice(s) => {
                // Simplified: treat as zero vector of that width
                format!("(tmdl_word_of_nat {} 0)", width)
            }
            _ => format!("(tmdl_word_of_nat {} 0)", width),
        };

        pieces.push((high_bit, piece));
    }

    // Sort by decreasing high_bit and concatenate
    pieces.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = String::new();
    for (i, (_hb, p)) in pieces.iter().enumerate() {
        if i > 0 {
            out.push_str(" ++ ");
        }
        out.push_str(p);
    }
    out
}

fn build_coq_behavior<'a>(
    item_cache: &HashMap<String, &'a Item>,
    instruction: &'a Instruction,
) -> String {
    let operands = resolve_operands_for_instruction(instruction, item_cache)
        .into_iter()
        .collect::<HashMap<_, _>>();
    let params = resolve_params_for_instruction(instruction, item_cache);
    let mut numeric_params = HashMap::new();
    for (name, (_ty, val)) in params {
        if let Some(ast::Expr::Lit(ast::Lit::Int(li))) = val {
            numeric_params.insert(name, parse_literal_value(&li) as i64);
        }
    }

    fn try_emit_sem_expr(
        e: &ast::Expr,
        operands: &HashMap<String, ast::Type>,
        numeric_params: &HashMap<String, i64>,
    ) -> Option<String> {
        let converted = convert_to_sem_expr(e, numeric_params.clone()).ok()?;
        let resolver = RocqSymbolResolver {
            symbols: &converted.symbols,
            operands,
            state_name: "st",
        };
        let mut out = Vec::new();
        sem_rocq::emit(&converted.expr, &mut out, &resolver).ok()?;
        String::from_utf8(out).ok()
    }

    // Legacy renderer used as fallback when semantic conversion cannot represent the AST.
    fn eval_expr_legacy(e: &ast::Expr, operands: &HashMap<String, ast::Type>) -> String {
        match e {
            ast::Expr::Lit(ast::Lit::Int(li)) => li.value().to_string(),
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
                let lhs = eval_expr_legacy(&b.lhs, operands);
                let rhs = eval_expr_legacy(&b.rhs, operands);
                let op = match b.op {
                    ast::BinOp::Add => "+",
                    ast::BinOp::Sub => "-",
                    ast::BinOp::Mul => "*",
                    ast::BinOp::Div => "/",
                    // Keep Lean-like bitwise ops as placeholders
                    ast::BinOp::BitwiseAnd => "&&&",
                    ast::BinOp::BitwiseOr => "|||",
                    ast::BinOp::BitwiseXor => "^^^",
                    ast::BinOp::ShiftLeftLogical => "<<<",
                    ast::BinOp::ShiftRightLogical => ">>>",
                    ast::BinOp::ShiftRightArithmetic => ">>>",
                };
                format!("({} {} {})", lhs, op, rhs)
            }
            ast::Expr::Slice(s) => {
                // Placeholder slice rendering; use base expr directly
                eval_expr_legacy(&s.base, operands)
            }
            ast::Expr::IndexAccess(s) => {
                // Placeholder index access; use base expr directly
                eval_expr_legacy(&s.base, operands)
            }
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
                        eval_expr_legacy(last, operands)
                    } else {
                        "0".to_string()
                    }
                } else {
                    "0".to_string()
                }
            }
            ast::Expr::Assign(_a) => {
                // Not an rvalue; fallback to 0
                "0".to_string()
            }
            ast::Expr::If(i) => {
                let c = eval_expr_legacy(&i.cond, operands);
                let t = eval_expr_legacy(&i.then, operands);
                let e = if let Some(e) = &i.else_ {
                    eval_expr_legacy(e, operands)
                } else {
                    "0".to_string()
                };
                format!("(if {} then {} else {})", c, t, e)
            }
            ast::Expr::BuiltinFunction(_) | ast::Expr::Call(_) | ast::Expr::Invalid => {
                "0".to_string()
            }
        }
    }

    fn eval_expr(
        e: &ast::Expr,
        operands: &HashMap<String, ast::Type>,
        numeric_params: &HashMap<String, i64>,
    ) -> String {
        if let Some(rendered) = try_emit_sem_expr(e, operands, numeric_params) {
            rendered
        } else {
            eval_expr_legacy(e, operands)
        }
    }

    // Compile a statement (assignment or block) into a state-transforming Coq expr
    fn compile_to_state(
        e: &ast::Expr,
        operands: &HashMap<String, ast::Type>,
        numeric_params: &HashMap<String, i64>,
        st_name: &str,
    ) -> String {
        match e {
            ast::Expr::Assign(a) => {
                let dst_name = a.dest.to_lowercase();
                let rhs = eval_expr(&a.value, operands, numeric_params);
                if let Some(ast::Type::Struct(rc)) = operands.get(&a.dest) {
                    format!(
                        "(write_{} {} {} {})",
                        rc.to_lowercase(),
                        st_name,
                        dst_name,
                        rhs
                    )
                } else {
                    // Non-register destination; no state change
                    st_name.to_string()
                }
            }
            ast::Expr::Block(b) => {
                let mut current = st_name.to_string();
                for stmt in &b.stmts {
                    match stmt {
                        ast::Expr::Assign(_) | ast::Expr::Block(_) | ast::Expr::If(_) => {
                            let next = compile_to_state(stmt, operands, numeric_params, &current);
                            current = next;
                        }
                        _ => {}
                    }
                }
                current
            }
            ast::Expr::If(i) => {
                let cond = eval_expr(&i.cond, operands, numeric_params);
                let then_state = compile_to_state(&i.then, operands, numeric_params, st_name);
                let else_state = if let Some(e) = &i.else_ {
                    compile_to_state(e, operands, numeric_params, st_name)
                } else {
                    st_name.to_string()
                };
                format!("(if {} then {} else {})", cond, then_state, else_state)
            }
            _ => st_name.to_string(),
        }
    }

    // Top-level behavior
    compile_to_state(&instruction.behavior, &operands, &numeric_params, "st")
}

const HEADER: &'static str = "(* Automatically generated by TMDL compiler *)
From Stdlib Require Import ZArith Lia.
Local Open Scope Z_scope.

Definition tmdl_modulus (bits : nat) : Z := 2 ^ (Z.of_nat bits).

Lemma tmdl_modulus_pos (bits : nat) : tmdl_modulus bits > 0.
Proof.
  unfold tmdl_modulus.
  apply Z.lt_gt.
  apply Z.pow_pos_nonneg; [lia | apply Zle_0_nat].
Qed.

Record tmdl_word (bits : nat) := {
  tmdl_word_val : Z;
  tmdl_word_range : (0 <= tmdl_word_val < tmdl_modulus bits)%Z
}.

Arguments tmdl_word_val {bits}.
Arguments tmdl_word_range {bits}.

Definition tmdl_word_mk (bits : nat) (x : Z) : tmdl_word bits :=
  let m := tmdl_modulus bits in
  {| tmdl_word_val := Z.modulo x m;
     tmdl_word_range := Z.mod_pos_bound x m (Z.gt_lt _ _ (tmdl_modulus_pos bits)) |}.

Definition tmdl_word_zero (bits : nat) : tmdl_word bits :=
  tmdl_word_mk bits 0.

Definition tmdl_word_of_nat (bits : nat) (n : nat) : tmdl_word bits :=
  tmdl_word_mk bits (Z.of_nat n).

Definition tmdl_word_add {bits} (a b : tmdl_word bits) : tmdl_word bits :=
  tmdl_word_mk bits (tmdl_word_val a + tmdl_word_val b).

Definition tmdl_word_sub {bits} (a b : tmdl_word bits) : tmdl_word bits :=
  tmdl_word_mk bits (tmdl_word_val a - tmdl_word_val b).

Definition tmdl_word_land {bits} (a b : tmdl_word bits) : tmdl_word bits :=
  tmdl_word_mk bits (Z.land (tmdl_word_val a) (tmdl_word_val b)).

Definition tmdl_word_lor {bits} (a b : tmdl_word bits) : tmdl_word bits :=
  tmdl_word_mk bits (Z.lor (tmdl_word_val a) (tmdl_word_val b)).

Definition tmdl_word_lxor {bits} (a b : tmdl_word bits) : tmdl_word bits :=
  tmdl_word_mk bits (Z.lxor (tmdl_word_val a) (tmdl_word_val b)).

Definition tmdl_word_shl {bits} (a b : tmdl_word bits) : tmdl_word bits :=
  tmdl_word_mk bits (Z.shiftl (tmdl_word_val a) (tmdl_word_val b)).

Definition tmdl_word_concat {bits1 bits2}
  (a : tmdl_word bits1) (b : tmdl_word bits2) : tmdl_word (bits1 + bits2) :=
  tmdl_word_mk (bits1 + bits2)
    (tmdl_word_val a * 2 ^ Z.of_nat bits2 + tmdl_word_val b).

Declare Scope tmdl_scope.
Local Open Scope tmdl_scope.

Local Infix \"+\"   := tmdl_word_add (at level 50, left associativity) : tmdl_scope.
Local Infix \"-\"   := tmdl_word_sub (at level 50, left associativity) : tmdl_scope.
Local Infix \"^^^\" := tmdl_word_lxor (at level 40, left associativity) : tmdl_scope.
Local Infix \"|||\" := tmdl_word_lor (at level 40, left associativity) : tmdl_scope.
Local Infix \"&&&\" := tmdl_word_land (at level 40, left associativity) : tmdl_scope.
Local Infix \"<<<\" := tmdl_word_shl (at level 35, no associativity) : tmdl_scope.
Local Infix \"++\"  := tmdl_word_concat (at level 60, right associativity) : tmdl_scope.
";
