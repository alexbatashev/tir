use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::{self, Item};
use crate::error::TMDLError;

pub fn generate_lean(files: Vec<ast::File>, mut output: Box<dyn Write>) -> Result<(), TMDLError> {
    // Build item cache
    let mut item_cache: HashMap<String, Item> = HashMap::new();
    for f in &files {
        for it in &f.items {
            item_cache.insert(it.name().to_string(), it.clone());
        }
    }

    // Collect union of operand names
    let mut operand_names: HashSet<String> = HashSet::new();
    for f in &files {
        for it in &f.items {
            match it {
                Item::Template(t) => {
                    for (n, _) in &t.operands {
                        operand_names.insert(n.clone());
                    }
                }
                Item::Instruction(i) => {
                    let ops = resolve_operands_for_instruction(i, &item_cache);
                    for (n, _) in ops {
                        operand_names.insert(n);
                    }
                }
                _ => {}
            }
        }
    }
    let mut operand_list: Vec<String> = operand_names.into_iter().collect();
    operand_list.sort();

    // Collect instructions
    let mut instructions = Vec::new();
    for f in &files {
        for it in &f.items {
            if let Item::Instruction(inst) = it {
                instructions.push(inst);
            }
        }
    }

    // Generate Lean file
    emit_header(&mut output)?;
    emit_helpers(&mut output)?;
    emit_state_type(&mut output)?;
    emit_fields_type(&mut output, &operand_list)?;

    for inst in &instructions {
        emit_instruction_encoder(&mut output, inst, &item_cache)?;
        emit_instruction_semantics(&mut output, inst, &item_cache)?;
    }

    writeln!(output, "end TMDL")?;
    Ok(())
}

pub fn generate_lean_adapter(files: &[ast::File], out_dir: &str) -> Result<(), TMDLError> {
    let path = std::path::Path::new(out_dir).join("TMDL_Adapter.lean");
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(w, "-- TMDL Sail adapter (parametric interface)")?;
    writeln!(w, "import Std")?;
    writeln!(w, "import TMDL")?;
    writeln!(w, "")?;
    writeln!(w, "namespace TMDL")?;
    writeln!(w, "")?;
    writeln!(w, "structure SailIFace where")?;
    writeln!(w, "  Instr     : Type")?;
    writeln!(w, "  RegState  : Type")?;
    writeln!(w, "  decode    : List Bool -> Option Instr")?;
    writeln!(w, "  execute   : Instr -> RegState -> RegState")?;
    writeln!(w, "  getPC     : RegState -> Int")?;
    writeln!(w, "  getX      : RegState -> Int -> Int")?;
    writeln!(w, "")?;
    writeln!(
        w,
        "def stateRel (XLEN : Nat) (SI : SailIFace) (sT : State) (sS : SI.RegState) : Prop :="
    )?;
    writeln!(
        w,
        "  SI.getPC sS = sT.pc ∧ sT.rf 0 = 0 ∧ ∀ (i : Int), 1 ≤ i ∧ i ≤ 31 -> SI.getX sS i = sT.rf i"
    )?;
    writeln!(w, "")?;

    // Emit statement skeletons dependent on instructions present
    let mut instructions = Vec::new();
    for f in files {
        for it in &f.items {
            if let Item::Instruction(inst) = it {
                instructions.push(inst);
            }
        }
    }
    for inst in instructions {
        let name = &inst.name;
        let lower_name = name.to_lowercase();
        writeln!(w, "-- Existence of decode for {}", name)?;
        writeln!(
            w,
            "theorem {}_decode_exists (XLEN : Nat) (SI : SailIFace) (f : Fields) :",
            lower_name
        )?;
        writeln!(
            w,
            "  ∃ i : SI.Instr, SI.decode (encode_{} f) = some i := by",
            name
        )?;
        writeln!(w, "  sorry")?;
        writeln!(w, "")?;
        writeln!(w, "-- One-step refinement for {}", name)?;
        writeln!(w, "theorem {}_refines", lower_name)?;
        writeln!(
            w,
            "  (XLEN : Nat) (SI : SailIFace) (sT : State) (sS : SI.RegState) (f : Fields) (i : SI.Instr) :"
        )?;
        writeln!(
            w,
            "  stateRel XLEN SI sT sS -> SI.decode (encode_{} f) = some i ->",
            name
        )?;
        writeln!(
            w,
            "  stateRel XLEN SI (sem_{} XLEN sT f) (SI.execute i sS) := by",
            lower_name
        )?;
        writeln!(w, "  sorry")?;
        writeln!(w, "")?;
    }

    writeln!(w, "end TMDL")?;
    Ok(())
}

pub fn generate_lean_instance(files: &[ast::File], out_dir: &str) -> Result<(), TMDLError> {
    use std::fmt::Write as _;
    let path = std::path::Path::new(out_dir).join("TMDL_Sail_Instance.lean");
    let mut buf = String::new();
    // Header + imports
    writeln!(buf, "import Std").unwrap();
    writeln!(buf, "import TMDL").unwrap();
    writeln!(buf, "import LeanRV64D.lean").unwrap();
    writeln!(buf, "import LeanRV64D.LeanRV64D.Sail.Sail").unwrap();
    writeln!(buf, "import LeanRV64D.LeanRV64D.Specialization").unwrap();
    writeln!(buf, "import LeanRV64D.LeanRV64D.Defs").unwrap();
    writeln!(buf, "import LeanRV64D.LeanRV64D.RiscvDecodeExt").unwrap();
    writeln!(buf, "import LeanRV64D.LeanRV64D.RiscvInstsEnd\n").unwrap();
    writeln!(buf, "open LeanRV64D.Functions").unwrap();
    writeln!(buf, "open Sail").unwrap();
    writeln!(buf, "open PreSail\n").unwrap();
    writeln!(buf, "set_option sorryAbort true\n").unwrap();
    writeln!(buf, "namespace TMDL\n").unwrap();
    // Helpers
    writeln!(buf, "def bv32OfBits (bs : List Bool) : BitVec 32 :=").unwrap();
    writeln!(buf, "  bs.enum.foldl (fun acc (p : Nat × Bool) =>").unwrap();
    writeln!(buf, "    let (i,b) := p; if b then acc ||| (BitVec.ofNat 32 (1 <<< i)) else acc) (0 : BitVec 32)\n").unwrap();
    writeln!(buf, "abbrev RegState := PreSail.SequentialState RegisterType trivialChoiceSource\n").unwrap();
    writeln!(buf, "def getPC (σ : RegState) : BitVec 64 := (σ.regs.get? Register.PC).getD 0").unwrap();
    writeln!(buf, "def getX (σ : RegState) (i : Int) : BitVec 64 :=").unwrap();
    writeln!(buf, "  match (Nat.toInt? i) with").unwrap();
    writeln!(buf, "  | none => 0").unwrap();
    writeln!(buf, "  | some n =>").unwrap();
    writeln!(buf, "    match n.toNat with").unwrap();
    for idx in 0..=31 {
        if idx == 0 {
            writeln!(buf, "    | 0  => 0").unwrap();
        } else {
            writeln!(buf, "    | {}  => (σ.regs.get? Register.x{}).getD 0", idx, idx).unwrap();
        }
    }
    writeln!(buf, "    | _  => 0\n").unwrap();
    writeln!(buf, "def stateRel64 (XLEN : Nat) (sT : State) (σ : RegState) : Prop :=").unwrap();
    writeln!(buf, "  BitVec.toInt (getPC σ) = sT.pc ∧ sT.rf 0 = 0 ∧").unwrap();
    writeln!(buf, "  (∀ (i : Int), 1 ≤ i ∧ i ≤ 31 -> BitVec.toInt (getX σ i) = sT.rf i)\n").unwrap();
    writeln!(buf, "def run (m : SailM α) (σ : RegState) : Except (Error exception) α × RegState :=").unwrap();
    writeln!(buf, "  let r := m.run σ; (r.result, r.state)\n").unwrap();

    // Per-instruction lemmas (auto-generated)
    let mut instructions = Vec::new();
    for f in files { for it in &f.items { if let Item::Instruction(i) = it { instructions.push(i); }}}
    for inst in instructions {
        let iname = &inst.name;
        let lname = iname.to_lowercase();
        writeln!(buf, "theorem {}_decode_exists_64 (f : Fields) (σ : RegState) :", lname).unwrap();
        writeln!(buf, "  ∃ i : instruction, (run (ext_decode (bv32OfBits (encode_{} f))) σ).fst = Except.ok i := by", iname).unwrap();
        writeln!(buf, "  -- auto-generated: computation over ext_decode and encode_{}", iname).unwrap();
        writeln!(buf, "  sorry\n").unwrap();

        writeln!(buf, "theorem {}_refines_64", lname).unwrap();
        writeln!(buf, "  (XLEN : Nat) (sT : State) (σ : RegState) (f : Fields) (i : instruction) :").unwrap();
        writeln!(buf, "  stateRel64 XLEN sT σ -> (run (ext_decode (bv32OfBits (encode_{} f))) σ).fst = Except.ok i ->", iname).unwrap();
        writeln!(buf, "  stateRel64 XLEN (sem_{} XLEN sT f) ((run (do let _ ← execute i; pure ()) σ).snd) := by", lname).unwrap();
        writeln!(buf, "  -- auto-generated: relies on execute semantics matching sem_{}", lname).unwrap();
        writeln!(buf, "  sorry\n").unwrap();
    }
    writeln!(buf, "end TMDL").unwrap();
    std::fs::write(path, buf)?;
    Ok(())
}

fn resolve_operands_for_instruction(
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
) -> HashMap<String, ast::Type> {
    let mut result = HashMap::new();

    fn collect_from_template(
        name: &str,
        cache: &HashMap<String, ast::Item>,
        acc: &mut HashMap<String, ast::Type>,
    ) {
        if let Some(Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.operands {
                acc.insert(k.clone(), v.clone());
            }
        }
    }

    if let Some(p) = &inst.parent_template {
        collect_from_template(p, cache, &mut result);
    }
    for (k, v) in &inst.operands {
        result.insert(k.clone(), v.clone());
    }
    result
}

fn emit_header(output: &mut dyn Write) -> Result<(), TMDLError> {
    writeln!(output, "-- Generated from TMDL")?;
    writeln!(
        output,
        "-- Lean semantics scaffold for instructions and proofs"
    )?;
    writeln!(output)?;
    writeln!(output, "import Std")?;
    writeln!(output)?;
    writeln!(output, "namespace TMDL")?;
    writeln!(output)?;
    Ok(())
}

fn emit_helpers(output: &mut dyn Write) -> Result<(), TMDLError> {
    writeln!(output, "-- Helpers")?;
    writeln!(output, "def pow2 (n : Nat) : Int := (2 : Int) ^ n")?;
    writeln!(
        output,
        "def modX (XLEN : Nat) (x : Int) : Int := Int.emod x (pow2 XLEN)"
    )?;
    writeln!(
        output,
        "def addX (XLEN : Nat) (a b : Int) : Int := modX XLEN (a + b)"
    )?;
    writeln!(
        output,
        "def subX (XLEN : Nat) (a b : Int) : Int := modX XLEN (a - b)"
    )?;
    writeln!(
        output,
        "def andX (XLEN : Nat) (a b : Int) : Int :=\n  let A := Int.toNat (modX XLEN a)\n  let B := Int.toNat (modX XLEN b)\n  Int.ofNat (Nat.land A B)"
    )?;
    writeln!(
        output,
        "def orX (XLEN : Nat) (a b : Int) : Int :=\n  let A := Int.toNat (modX XLEN a)\n  let B := Int.toNat (modX XLEN b)\n  Int.ofNat (Nat.lor A B)"
    )?;
    writeln!(
        output,
        "def xorX (XLEN : Nat) (a b : Int) : Int :=\n  let A := Int.toNat (modX XLEN a)\n  let B := Int.toNat (modX XLEN b)\n  Int.ofNat (Nat.xor A B)"
    )?;
    writeln!(
        output,
        "def shlX (XLEN : Nat) (a n : Int) : Int :=\n  let A := Int.toNat (modX XLEN a)\n  let N := Int.toNat n\n  modX XLEN (Int.ofNat (Nat.shiftLeft A N))"
    )?;
    writeln!(
        output,
        "def lshrX (XLEN : Nat) (a n : Int) : Int :=\n  let A := Int.toNat (modX XLEN a)\n  let N := Int.toNat n\n  Int.ofNat (Nat.shiftRight A N)"
    )?;
    writeln!(
        output,
        "-- NOTE: Arithmetic shift right approximated by logical shift right for now."
    )?;
    writeln!(
        output,
        "def ashrX (XLEN : Nat) (a n : Int) : Int := lshrX XLEN a n"
    )?;
    writeln!(output, "-- Bit helpers for encoders (LSB-first lists)")?;
    writeln!(output, "def toBits (len : Nat) (x : Int) : List Bool :=")?;
    writeln!(
        output,
        "  let rec go (k : Nat) (n : Nat) (acc : List Bool) : List Bool :="
    )?;
    writeln!(output, "    match k with")?;
    writeln!(output, "    | 0   => acc")?;
    writeln!(output, "    | k+1 =>")?;
    writeln!(
        output,
        "      let bit : Bool := decide ((Nat.land n 1) = 1)"
    )?;
    writeln!(output, "      go k (Nat.shiftRight n 1) (acc ++ [bit])")?;
    writeln!(output, "  go len (Int.toNat x) []")?;
    writeln!(output, "")?;
    writeln!(
        output,
        "def setRange (w : List Bool) (hi lo : Nat) (src : List Bool) : List Bool :="
    )?;
    writeln!(output, "  let pre  := w.take lo")?;
    writeln!(output, "  let post := w.drop (hi + 1)")?;
    writeln!(output, "  let need := hi - lo + 1")?;
    writeln!(output, "  pre ++ src.take need ++ post")?;
    writeln!(output)?;
    Ok(())
}

fn emit_state_type(output: &mut dyn Write) -> Result<(), TMDLError> {
    writeln!(output, "-- Machine state")?;
    writeln!(output, "structure State where")?;
    writeln!(output, "  pc : Int")?;
    writeln!(
        output,
        "  rf : Int -> Int  -- Register file, x0..x31 indices"
    )?;
    writeln!(output)?;
    Ok(())
}

fn emit_fields_type(output: &mut dyn Write, operand_list: &[String]) -> Result<(), TMDLError> {
    writeln!(output, "-- Instruction operand fields")?;
    writeln!(output, "structure Fields where")?;
    for operand in operand_list {
        writeln!(output, "  {} : Int", operand)?;
    }
    writeln!(output)?;
    Ok(())
}

fn emit_instruction_semantics(
    output: &mut dyn Write,
    inst: &ast::Instruction,
    item_cache: &HashMap<String, ast::Item>,
) -> Result<(), TMDLError> {
    let name = &inst.name;
    let lower_name = name.to_lowercase();

    writeln!(output, "-- Semantics for {}", name)?;
    writeln!(
        output,
        "def sem_{} (XLEN : Nat) (s : State) (f : Fields) : State :=",
        lower_name
    )?;

    let pc_inc_bytes = infer_encoding_bytes(inst, item_cache);
    let has_rd = has_register_dest(inst, item_cache);
    let expr_str = lean_expr(&inst.behavior, inst, item_cache).unwrap_or_else(|_| "0".to_string());

    writeln!(
        output,
        "  let s1 := {{ s with pc := s.pc + {} }}",
        pc_inc_bytes
    )?;
    if has_rd {
        writeln!(
            output,
            "  let s2 := if f.rd = 0 then s1 else {{ s1 with rf := fun i => if i = f.rd then modX XLEN ({}) else s1.rf i }}",
            expr_str
        )?;
        writeln!(output, "  s2")?;
    } else {
        writeln!(output, "  s1")?;
    }

    writeln!(output)?;
    Ok(())
}

fn emit_instruction_encoder(
    output: &mut dyn Write,
    inst: &ast::Instruction,
    item_cache: &HashMap<String, ast::Item>,
) -> Result<(), TMDLError> {
    use ast::{EncodingArm, Item};

    // Resolve all encoding arms from template lineage + instruction
    let mut cache: HashMap<String, ast::Item> = HashMap::new();
    for (_, it) in item_cache.iter() {
        cache.insert(it.name().to_string(), it.clone());
    }
    let mut encs: Vec<EncodingArm> = Vec::new();
    let mut cur = inst.parent_template.clone();
    while let Some(name) = cur {
        cur = match cache.get(&name) {
            Some(Item::Template(t)) => {
                encs.extend(t.encoding.clone());
                t.parent_template.clone()
            }
            _ => None,
        };
    }
    encs.extend(inst.encoding.clone());
    // Compute total width
    let mut width: u16 = 0;
    for a in &encs {
        let hi = a.end.unwrap_or(a.start);
        width = width.max(hi + 1);
    }

    let name = &inst.name;
    writeln!(output, "-- Encoder for {}", name)?;
    writeln!(output, "def encode_{} (f : Fields) : List Bool :=", name)?;
    writeln!(output, "  let w0 := List.replicate {} false", width)?;
    for (i, a) in encs.iter().enumerate() {
        let idx = i + 1;
        let lo = a.start as u32;
        let hi = a.end.unwrap_or(a.start) as u32;
        let len = (hi - lo + 1) as u32;
        let src = lean_encode_expr(&a.value, inst, &cache)?;
        writeln!(
            output,
            "  let w{} := setRange w{} {} {} (toBits {} ({}))",
            idx,
            idx - 1,
            hi,
            lo,
            len,
            src
        )?;
    }
    writeln!(output, "  w{}\n", encs.len())?;
    Ok(())
}

fn lean_encode_expr(
    e: &ast::Expr,
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
) -> Result<String, TMDLError> {
    use ast::{Expr, Lit};
    match e {
        Expr::Lit(Lit::Int(i)) => Ok(i.value().to_string()),
        Expr::Lit(Lit::Str(_)) => Err(TMDLError::UnexpectedExpression),
        Expr::Ident(id) => {
            let ops = resolve_operands_for_instruction(inst, cache);
            if ops.contains_key(&id.name) {
                Ok(format!("f.{}", id.name))
            } else if let Some(v) = resolve_param_value(inst, cache, &id.name) {
                Ok(v)
            } else {
                Ok(format!("f.{}", id.name))
            }
        }
        Expr::Slice(s) => match &*s.base {
            Expr::Ident(id) => {
                let ops = resolve_operands_for_instruction(inst, cache);
                if ops.contains_key(&id.name) {
                    Ok(format!("(f.{} / pow2 {})", id.name, s.start))
                } else if let Some(v) = resolve_param_value(inst, cache, &id.name) {
                    Ok(format!("({} / pow2 {})", v, s.start))
                } else {
                    Ok(format!("(f.{} / pow2 {})", id.name, s.start))
                }
            }
            _ => Err(TMDLError::UnexpectedExpression),
        },
        Expr::IndexAccess(ia) => match &*ia.base {
            Expr::Ident(id) => {
                let ops = resolve_operands_for_instruction(inst, cache);
                if ops.contains_key(&id.name) {
                    Ok(format!("(f.{} / pow2 {})", id.name, ia.index))
                } else if let Some(v) = resolve_param_value(inst, cache, &id.name) {
                    Ok(format!("({} / pow2 {})", v, ia.index))
                } else {
                    Ok(format!("(f.{} / pow2 {})", id.name, ia.index))
                }
            }
            _ => Err(TMDLError::UnexpectedExpression),
        },
        _ => Err(TMDLError::UnexpectedExpression),
    }
}

fn resolve_param_value(
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
    name: &str,
) -> Option<String> {
    use ast::{Expr, Item, Lit};
    // Collect lineage templates root-first
    let mut lineage: Vec<String> = Vec::new();
    let mut cur = inst.parent_template.clone();
    while let Some(tn) = cur {
        lineage.push(tn.clone());
        cur = match cache.get(&tn) {
            Some(Item::Template(t)) => t.parent_template.clone(),
            _ => None,
        };
    }
    lineage.reverse();
    // template defaults
    let mut val: Option<Expr> = None;
    for tn in lineage {
        if let Some(Item::Template(t)) = cache.get(&tn) {
            if let Some((_ty, maybe)) = t.params.get(name) {
                if let Some(e) = maybe {
                    val = Some(e.clone());
                }
            }
        }
    }
    // instruction overrides
    if let Some((_ty, maybe)) = inst.params.get(name) {
        if let Some(e) = maybe {
            val = Some(e.clone());
        }
    }
    match val {
        Some(Expr::Lit(Lit::Int(i))) => Some(i.value().to_string()),
        _ => None,
    }
}

fn infer_encoding_bytes(inst: &ast::Instruction, cache: &HashMap<String, ast::Item>) -> u16 {
    // Determine instruction width from encoding arms
    let mut encs: Vec<ast::EncodingArm> = Vec::new();
    let mut cur = inst.parent_template.clone();
    while let Some(name) = cur {
        cur = match cache.get(&name) {
            Some(Item::Template(t)) => {
                encs.extend(t.encoding.clone());
                t.parent_template.clone()
            }
            _ => None,
        };
    }
    encs.extend(inst.encoding.clone());
    let mut width: u16 = 0;
    for a in &encs {
        let hi = a.end.unwrap_or(a.start);
        width = width.max(hi + 1);
    }
    // Return bytes (assume width is multiple of 8)
    (width / 8).max(2)
}

fn has_register_dest(inst: &ast::Instruction, cache: &HashMap<String, ast::Item>) -> bool {
    let ops = resolve_operands_for_instruction(inst, cache);
    if let Some(ast::Type::Struct(_)) = ops.get("rd") {
        true
    } else {
        false
    }
}

fn lean_expr(
    e: &ast::Expr,
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
) -> Result<String, TMDLError> {
    use ast::{BinOp, Binary, Expr, Lit, Type};
    let ops = resolve_operands_for_instruction(inst, cache);
    let mut resolve_ident = |id: &str| -> String {
        match ops.get(id) {
            Some(Type::Struct(_)) => format!("s.rf f.{}", id),
            Some(Type::Bits(_)) | Some(Type::Integer) => format!("f.{}", id),
            _ => format!("f.{}", id),
        }
    };
    match e {
        Expr::Assign(a) => lean_expr(&a.value, inst, cache),
        Expr::Block(b) => {
            // return last expression value
            let mut last: Option<String> = None;
            for s in &b.stmts {
                last = Some(lean_expr(s, inst, cache)?);
            }
            Ok(last.unwrap_or_else(|| "0".to_string()))
        }
        Expr::Ident(id) => Ok(resolve_ident(&id.name)),
        Expr::Lit(Lit::Int(i)) => Ok(i.value().to_string()),
        Expr::Lit(Lit::Str(_)) => Err(TMDLError::UnexpectedExpression),
        Expr::Binary(Binary { lhs, rhs, op, .. }) => {
            let l = lean_expr(lhs, inst, cache)?;
            let r = lean_expr(rhs, inst, cache)?;
            let s = match op {
                BinOp::Add => format!("addX XLEN ({}) ({})", l, r),
                BinOp::Sub => format!("subX XLEN ({}) ({})", l, r),
                BinOp::BitwiseAnd => format!("andX XLEN ({}) ({})", l, r),
                BinOp::BitwiseOr => format!("orX XLEN ({}) ({})", l, r),
                BinOp::BitwiseXor => format!("xorX XLEN ({}) ({})", l, r),
                BinOp::ShiftLeftLogical => format!("shlX XLEN ({}) ({})", l, r),
                BinOp::ShiftRightLogical => format!("lshrX XLEN ({}) ({})", l, r),
                BinOp::ShiftRightArithmetic => format!("ashrX XLEN ({}) ({})", l, r),
                BinOp::Mul | BinOp::Div => return Err(TMDLError::UnexpectedExpression),
            };
            Ok(s)
        }
        Expr::If(i) => {
            let c = lean_expr(&i.cond, inst, cache)?;
            let t = lean_expr(&i.then, inst, cache)?;
            let e2 = if let Some(e) = &i.else_ {
                lean_expr(e, inst, cache)?
            } else {
                "0".to_string()
            };
            Ok(format!("(if {} != 0 then ({}) else ({}))", c, t, e2))
        }
        Expr::Slice(_) | Expr::IndexAccess(_) | Expr::Call(_) => {
            // Not yet supported in Lean backend
            Err(TMDLError::UnexpectedExpression)
        }
        Expr::Field(_) | Expr::Invalid => Err(TMDLError::UnexpectedExpression),
    }
}
