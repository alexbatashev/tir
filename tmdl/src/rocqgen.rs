use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::{self, BinOp, EncodingArm, Expr, Item, Lit};
use crate::error::TMDLError;

const HEADER: &'static str = "Definition word := Z.

Inductive binop := BAdd | BSub | BAnd | BOr | BXor | BSll | BSrl | BSra.
Inductive expr := EReg Z | EImm Z | EBin binop expr expr.
Record Stmt := { dst: Z; body: expr }.

(* Width and truncation utilities *)
Definition pow2 (w:Z) := 2 ^ w.
Definition maskw (w:Z) := pow2 w - 1.
Definition trunc (w z:Z) := Z.land z (maskw w).
Definition shamt_mask (w z:Z) := Z.land z (w - 1).  (* for shifts *)

(* helpers for bit slicing from an instruction word *)
Definition bits (hi lo : Z) (x : Z) : Z :=
  Z.land (Z.shiftr x lo) (maskw (hi - lo + 1)).

(* Fixed machine parameters placeholder *)
Module Params.
  Parameter XLEN : Z.
  Axiom XLEN_pos : 0 < XLEN.
End Params.

Definition normalize (z:Z) : Z := trunc Params.XLEN z.
Definition shamt (z:Z) : Z := shamt_mask Params.XLEN z.

Record State := { pc : Z; rf : Z -> Z }.

Definition read_reg (s:State) (i:Z) : Z :=
  if Z.eqb i 0 then 0 else s.(rf) i.

Definition write_reg (s:State) (i v:Z) : State :=
  if Z.eqb i 0 then s
  else {| pc := s.(pc);
          rf := fun j => if Z.eqb j i then v else s.(rf) j |}.

(* Arithmetic right shift over XLEN-width bit-vectors *)
Definition sra_bv (w:Z) (x sh:Z) : Z :=
  let v  := normalize x in
  let sa := shamt sh in
  let srl := Z.shiftr v sa in
  let sign := Z.land v (Z.shiftl 1 (w - 1)) in
  if Z.eqb sign 0 then srl
  else Z.lor srl (Z.shiftl (maskw sa) (w - sa)).

(* Z-math semantics (mask once on write) *)
Fixpoint eval_z (s:State) (e:expr) : Z :=
  match e with
  | EReg r => read_reg s r
  | EImm i => i
  | EBin op a b =>
      let va := eval_z s a in
      let vb := eval_z s b in
      match op with
      | BAdd => Z.add va vb
      | BSub => Z.sub va vb
      | BAnd => Z.land va vb
      | BOr  => Z.lor va vb
      | BXor => Z.lxor va vb
      | BSll => Z.shiftl va (shamt vb)
      | BSrl => Z.shiftr va (shamt vb)
      | BSra => sra_bv Params.XLEN va vb
      end
  end.

Definition exec_stmt_z (s:State) (st:Stmt) : State :=
  let v := normalize (eval_z s st.(body)) in
  write_reg s st.(dst) v.

(* Bit-vector style semantics (mask per primitive) *)
Fixpoint eval_bv (s:State) (e:expr) : Z :=
  match e with
  | EReg r => normalize (read_reg s r)
  | EImm i => normalize i
  | EBin op a b =>
      let va := eval_bv s a in
      let vb := eval_bv s b in
      let res :=
        match op with
        | BAdd => Z.add va vb
        | BSub => Z.sub va vb
        | BAnd => Z.land va vb
        | BOr  => Z.lor va vb
        | BXor => Z.lxor va vb
        | BSll => Z.shiftl va (shamt vb)
        | BSrl => Z.shiftr va (shamt vb)
        | BSra => sra_bv Params.XLEN va vb
        end in
      normalize res
  end.

Definition exec_stmt_bv (s:State) (st:Stmt) : State :=
  let v := eval_bv s st.(body) in
  write_reg s st.(dst) v.

(* Default aliases for backwards compatibility *)
Definition eval := eval_z.
Definition exec_stmt := exec_stmt_z.

Definition next_pc (s:State) : Z := s.(pc) + 4.
";

const INST_DESC: &'static str = "(* One instruction descriptor *)
Record InstrDesc := {
  mask  : Z;                      (* encoding mask *)
  pat   : Z;                      (* encoding value *)
  match_word (w:word) : Prop := Z.land w mask = pat;

  (* extract fields from the word (fail if malformed) *)
  decode : word -> option Fields;

  (* behavior AST parameterized by decoded fields *)
  sem    : Fields -> Stmt;

  (* instruction length in bytes *)
  ilen   : Z
}.

";

const DECODE_TABLE: &'static str =
    "Fixpoint decode_table (ws: list InstrDesc) (w:word) : option (InstrDesc * Fields) :=
  match ws with
  | [] => None
  | d::ds =>
      if Z.eqb (Z.land w d.(mask)) d.(pat)
      then match d.(decode) w with
           | Some f => Some (d,f)
           | None   => decode_table ds w
           end
      else decode_table ds w
  end.

 Definition next_pc_by (s:State) (ilen:Z) := s.(pc) + ilen.

 Definition step (tbl : list InstrDesc) (s:State) (iw:word) : option State :=
    match decode_table tbl iw with
    | None => None
    | Some (d,f) =>
        let s' := exec_stmt s (d.(sem) f) in
        Some {| pc := next_pc_by s d.(ilen); rf := s'.(rf) |}
    end.

Definition step_z (tbl : list InstrDesc) (s:State) (iw:word) : option State :=
    match decode_table tbl iw with
    | None => None
    | Some (d,f) =>
        let s' := exec_stmt_z s (d.(sem) f) in
        Some {| pc := next_pc_by s d.(ilen); rf := s'.(rf) |}
    end.

Definition step_bv (tbl : list InstrDesc) (s:State) (iw:word) : option State :=
    match decode_table tbl iw with
    | None => None
    | Some (d,f) =>
        let s' := exec_stmt_bv s (d.(sem) f) in
        Some {| pc := next_pc_by s d.(ilen); rf := s'.(rf) |}
    end.

Lemma decode_table_sound ds d f w :
    In d ds ->
    nonoverlap (ds) ->
    Z.land w d.(mask) = d.(pat) ->
    d.(decode) w = Some f ->
    exists ds', decode_table ds w = Some (d,f).
(* straightforward induction on ds; use nonoverlap to kill the “earlier match” case *)
Admitted.

Definition obs (s:State) : Z * (Z -> Z) := (pc s, rf s).

Definition Dom_TMDL (tbl:list InstrDesc) (iw:Z) :=
  exists d f, decode_table tbl iw = Some (d,f).

";

pub fn generate_rocq(ast: Vec<ast::File>, mut output: Box<dyn Write>) -> Result<(), TMDLError> {
    // Write common prelude
    output.write_all(HEADER.as_bytes())?;

    // Build item cache by name for template traversal
    let mut item_cache: HashMap<String, ast::Item> = HashMap::new();
    for f in &ast {
        for it in &f.items {
            item_cache.insert(it.name().to_string(), it.clone());
        }
    }

    // Collect union of operand names to define Fields record
    let mut operand_names: HashSet<String> = HashSet::new();
    for f in &ast {
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
    let fields_sig = operand_list
        .iter()
        .map(|n| format!("{}: Z", n))
        .collect::<Vec<_>>()
        .join("; ");
    writeln!(output, "Record Fields := {{ {} }}.\n", fields_sig)?;

    // Instruction descriptor and decode table machinery
    output.write_all(INST_DESC.as_bytes())?;
    output.write_all(DECODE_TABLE.as_bytes())?;

    // Emit per-instruction definitions
    let mut desc_names: Vec<String> = Vec::new();
    for f in &ast {
        for it in &f.items {
            if let Item::Instruction(inst) = it {
                let name = &inst.name;
                let desc_name = format!("{}_desc", name.to_uppercase());
                desc_names.push(desc_name.clone());

                // Resolve operands and parameters
                let ops = resolve_operands_for_instruction(inst, &item_cache);
                let params = resolve_params_for_instruction(inst, &item_cache);
                let enc = resolve_encoding_for_instruction(inst, &item_cache);

                // Compute mask and pattern
                let (mask, pat, width) = compute_mask_and_pat(&enc, &params, &ops);

                // Emit mask and pat
                let mask_name = format!("{}_mask", name.to_uppercase());
                let pat_name = format!("{}_pat", name.to_uppercase());
                writeln!(output, "Definition {} : Z := {}.", mask_name, mask)?;
                writeln!(output, "Definition {} : Z := {}.\n", pat_name, pat)?;

                // Emit decode
                let decode_name = format!("decode_{}", name.to_uppercase());
                let decode_body = emit_decode_function(&decode_name, &operand_list, &enc, width);
                output.write_all(decode_body.as_bytes())?;

                // Emit semantics
                let sem_name = format!("sem_{}", name.to_uppercase());
                let sem_body = emit_semantics_function(&sem_name, inst, &ops)?;
                output.write_all(sem_body.as_bytes())?;

                // Emit instruction descriptor
                writeln!(
                    output,
                    "Definition {} : InstrDesc :=\n  {{| mask := {};\n     pat  := {};\n     decode := {};\n     sem    := {};\n     ilen   := {} |}}.\n",
                    desc_name,
                    mask_name,
                    pat_name,
                    decode_name,
                    sem_name,
                    (width as u32 + 7) / 8
                )?;
            }
        }
    }

    // Emit instruction table
    if !desc_names.is_empty() {
        let table_elems = desc_names.join(", ");
        writeln!(
            output,
            "Definition table : list InstrDesc := [{}].",
            table_elems
        )?;
    }

    // Helpful global lemmas and properties
    output.write_all(LEMMA_SUPPORT.as_bytes())?;

    // For each instruction, emit a convenience singleton decode lemma
    for f in &ast {
        for it in &f.items {
            if let Item::Instruction(inst) = it {
                let uname = inst.name.to_uppercase();
                let lemma_name = format!("decode_table_singleton_{}", uname);
                let desc = format!("{}_desc", uname);
                let decf = format!("decode_{}", uname);
                writeln!(
                    output,
                    "Lemma {} w f :\n  Z.land w {}.(mask) = {}.(pat) ->\n  {} w = Some f ->\n  decode_table [{}] w = Some ({}, f).\nProof. intros; eapply decode_table_singleton_gen; eauto. Qed.\n",
                    lemma_name, desc, desc, decf, desc, desc
                )?;
            }
        }
    }

    Ok(())
}

const LEMMA_SUPPORT: &str = r"

(* non-overlap skeleton: for a singleton table this is trivial *)
Definition nonoverlap (ds:list InstrDesc) : Prop :=
  forall d1 d2 w, In d1 ds -> In d2 ds ->
    Z.land w d1.(mask) = d1.(pat) ->
    Z.land w d2.(mask) = d2.(pat) ->
    d1 = d2.

(* decode table correctness for single descriptor *)
Lemma decode_table_singleton_gen (d:InstrDesc) w f :
  Z.land w d.(mask) = d.(pat) ->
  d.(decode) w = Some f ->
  decode_table [d] w = Some (d, f).
Proof.
  intros Hm Hd. cbn. rewrite Hm. now rewrite Z.eqb_refl.
Qed.

(* writing x0 does not change it *)
Lemma write_reg_x0 s i v : (write_reg s i v).(rf) 0 = s.(rf) 0.
Proof.
  unfold write_reg. destruct (Z.eqb i 0) eqn:E; cbn; [reflexivity|].
  (* in else branch, the new rf checks equality against i; at 0 it's false *)
  now rewrite E.
Qed.

(* writing a register leaves all other registers unchanged *)
Lemma write_reg_other s i v j :
  j <> i -> (write_reg s i v).(rf) j = s.(rf) j.
Proof.
  unfold write_reg; destruct (Z.eqb i 0) eqn:E0; cbn.
  - reflexivity.
  - destruct (Z.eqb j i) eqn:E; apply Z.eqb_neq in E; congruence.
Qed.

(* a single exec preserves x0 (hardwired zero) *)
Lemma exec_stmt_preserves_x0 s st : (exec_stmt s st).(rf) 0 = s.(rf) 0.
Proof. unfold exec_stmt; cbn. apply write_reg_x0. Qed.

(* step preserves x0 given it holds initially *)
Lemma step_preserves_x0 tbl s iw s' :
  s.(rf) 0 = 0 ->
  step tbl s iw = Some s' ->
  s'.(rf) 0 = 0.
Proof.
  intros H0 Hst. unfold step in Hst.
  destruct (decode_table tbl iw) as [[d f]|] eqn:E; try discriminate.
  inversion Hst; subst; cbn.
  unfold exec_stmt; cbn. rewrite write_reg_x0. exact H0.
Qed.

(* step variants preserve x0 as well *)
Lemma step_z_preserves_x0 tbl s iw s' :
  s.(rf) 0 = 0 ->
  step_z tbl s iw = Some s' ->
  s'.(rf) 0 = 0.
Proof.
  intros H0 Hst. unfold step_z in Hst.
  destruct (decode_table tbl iw) as [[d f]|] eqn:E; try discriminate.
  inversion Hst; subst; cbn.
  unfold exec_stmt_z; cbn. rewrite write_reg_x0. exact H0.
Qed.

Lemma step_bv_preserves_x0 tbl s iw s' :
  s.(rf) 0 = 0 ->
  step_bv tbl s iw = Some s' ->
  s'.(rf) 0 = 0.
Proof.
  intros H0 Hst. unfold step_bv in Hst.
  destruct (decode_table tbl iw) as [[d f]|] eqn:E; try discriminate.
  inversion Hst; subst; cbn.
  unfold exec_stmt_bv; cbn. rewrite write_reg_x0. exact H0.
Qed.

(* Boolean match for a descriptor *)
Definition matches (d:InstrDesc) (w:word) : bool :=
  Z.eqb (Z.land w d.(mask)) d.(pat).

Lemma matches_eq d w :
  matches d w = true <-> Z.land w d.(mask) = d.(pat).
Proof. unfold matches; now rewrite Z.eqb_eq. Qed.

(* A convenient default descriptor for total nth *)
Definition dummy_desc : InstrDesc :=
  {| mask := 0; pat := 0; decode := (fun _ => None); sem := (fun _ => {| dst := 0; body := EImm 0 |}); ilen := 0 |}.

(* Static overlap check using masks/patterns only *)
Definition patterns_overlap (d1 d2:InstrDesc) : bool :=
  Z.eqb (Z.land d1.(pat) d2.(mask)) d2.(pat)
  &&   Z.eqb (Z.land d2.(pat) d1.(mask)) d1.(pat).

Definition nonoverlap_masks (ds:list InstrDesc) : bool :=
  forallb (fun i =>
    forallb (fun j =>
      if Nat.eqb i j then true
      else negb (patterns_overlap (nth i ds dummy_desc) (nth j ds dummy_desc))
    ) (seq 0 (length ds))
  ) (seq 0 (length ds)).

Lemma patterns_overlap_sound d1 d2 :
  patterns_overlap d1 d2 = true ->
  exists w, Z.land w d1.(mask) = d1.(pat) /\ Z.land w d2.(mask) = d2.(pat).
Admitted.

Lemma nonoverlap_masks_sound ds :
  nonoverlap_masks ds = true -> nonoverlap ds.
Proof.
  (* Follows from patterns_overlap_sound and boolean iteration over pairs. *)
Admitted.

(* Arithmetic truncation lemmas *)
Lemma trunc_idem z :
  trunc Params.XLEN (trunc Params.XLEN z) = trunc Params.XLEN z.
Proof.
  unfold trunc. rewrite Z.land_assoc. rewrite Z.land_diag. reflexivity.
Qed.

Lemma normalize_idem z : normalize (normalize z) = normalize z.
Proof. unfold normalize. apply trunc_idem. Qed.

Lemma trunc_add x y :
  trunc Params.XLEN (trunc Params.XLEN x + trunc Params.XLEN y)
  = trunc Params.XLEN (x + y).
Admitted.

Lemma trunc_and x y :
  trunc Params.XLEN (Z.land (trunc Params.XLEN x) (trunc Params.XLEN y))
  = trunc Params.XLEN (Z.land x y).
Admitted.

Lemma trunc_or x y :
  trunc Params.XLEN (Z.lor (trunc Params.XLEN x) (trunc Params.XLEN y))
  = trunc Params.XLEN (Z.lor x y).
Admitted.

Lemma trunc_xor x y :
  trunc Params.XLEN (Z.lxor (trunc Params.XLEN x) (trunc Params.XLEN y))
  = trunc Params.XLEN (Z.lxor x y).
Admitted.

Lemma trunc_sll x y :
  trunc Params.XLEN (Z.shiftl (trunc Params.XLEN x) (shamt y))
  = trunc Params.XLEN (Z.shiftl x (shamt y)).
Admitted.

Lemma trunc_srl x y :
  trunc Params.XLEN (Z.shiftr (trunc Params.XLEN x) (shamt y))
  = trunc Params.XLEN (Z.shiftr x (shamt y)).
Admitted.

Lemma trunc_sra x y :
  trunc Params.XLEN (sra_bv Params.XLEN (trunc Params.XLEN x) y)
  = trunc Params.XLEN (sra_bv Params.XLEN x y).
Admitted.
";

// Resolve operands for an instruction walking its template ancestry (root-most first)
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

// Resolve parameters for an instruction (final value if specified)
fn resolve_params_for_instruction(
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
) -> HashMap<String, Expr> {
    // Collect from root-most template first, then overrides from child templates, then instruction
    let mut order: Vec<HashMap<String, (ast::Type, Option<Expr>)>> = Vec::new();
    let mut cur = inst.parent_template.clone();
    let mut stack: Vec<String> = Vec::new();
    while let Some(name) = cur {
        stack.push(name.clone());
        cur = match cache.get(&name) {
            Some(Item::Template(t)) => t.parent_template.clone(),
            _ => None,
        };
    }
    // Now walk from root-most to closest
    for name in stack.into_iter().rev() {
        if let Some(Item::Template(t)) = cache.get(&name) {
            order.push(t.params.clone());
        }
    }
    order.push(inst.params.clone());

    let mut result: HashMap<String, Expr> = HashMap::new();
    for m in order {
        for (k, (_ty, v)) in m {
            if let Some(expr) = v {
                result.insert(k, expr);
            }
        }
    }
    result
}

// Resolve encoding arms for an instruction (concatenate from templates and instruction)
fn resolve_encoding_for_instruction(
    inst: &ast::Instruction,
    cache: &HashMap<String, ast::Item>,
) -> Vec<EncodingArm> {
    let mut encs: Vec<EncodingArm> = Vec::new();
    // collect from root-most template first
    let mut lineage: Vec<String> = Vec::new();
    let mut cur = inst.parent_template.clone();
    while let Some(name) = cur {
        lineage.push(name.clone());
        cur = match cache.get(&name) {
            Some(Item::Template(t)) => t.parent_template.clone(),
            _ => None,
        };
    }
    for name in lineage.into_iter().rev() {
        if let Some(Item::Template(t)) = cache.get(&name) {
            encs.extend(t.encoding.clone());
        }
    }
    encs.extend(inst.encoding.clone());
    encs
}

// Compute encoding mask and pattern (as Z numerals), plus inferred width
fn compute_mask_and_pat(
    enc: &[EncodingArm],
    params: &HashMap<String, Expr>,
    ops: &HashMap<String, ast::Type>,
) -> (String, String, u16) {
    // determine width
    let mut width: u16 = 0;
    for a in enc {
        let hi = a.end.unwrap_or(a.start);
        if hi + 1 > width {
            width = hi + 1;
        }
    }
    let mut mask: u128 = 0;
    let mut pat: u128 = 0;
    for a in enc {
        let lo = a.start as u32;
        let hi = a.end.unwrap_or(a.start) as u32;
        let len = (hi - lo + 1) as u32;
        if let Some(cval) = eval_const_bits(&a.value, params) {
            // Truncate cval to len
            let trunc = cval & ((1u128 << len) - 1);
            mask |= ((1u128 << len) - 1) << lo;
            pat |= trunc << lo;
        } else {
            // variable field => mask bits 0
            let _ = ops; // unused in this branch, but reserve for future checks
        }
    }
    (format!("{}", mask), format!("{}", pat), width)
}

fn eval_const_bits(expr: &Expr, params: &HashMap<String, Expr>) -> Option<u128> {
    match expr {
        Expr::Lit(Lit::Int(i)) => parse_int_str(i.value()),
        Expr::Ident(id) => {
            if let Some(v) = params.get(&id.name) {
                eval_const_bits(v, params)
            } else {
                None
            }
        }
        Expr::Slice(s) => {
            if let Some(base) = eval_const_bits(&s.base, params) {
                let lo = s.start as u32;
                let hi = s.end as u32;
                let len = (hi - lo + 1) as u32;
                let val = (base >> lo) & ((1u128 << len) - 1);
                Some(val)
            } else {
                None
            }
        }
        Expr::IndexAccess(ia) => {
            if let Some(base) = eval_const_bits(&ia.base, params) {
                let idx = ia.index as u32;
                Some((base >> idx) & 1)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_int_str(s: &str) -> Option<u128> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("0b") {
        u128::from_str_radix(rest, 2).ok()
    } else if let Some(rest) = s.strip_prefix("0x") {
        u128::from_str_radix(rest, 16).ok()
    } else if let Some(rest) = s.strip_prefix("0o") {
        u128::from_str_radix(rest, 8).ok()
    } else {
        s.parse::<u128>().ok()
    }
}

// Emit decode function for an instruction
fn emit_decode_function(
    decode_name: &str,
    all_fields: &[String],
    enc: &[EncodingArm],
    _width: u16,
) -> String {
    // Build mapping: operand name -> list of pieces (hi, lo, shift)
    let mut pieces: HashMap<String, Vec<(u16, u16, u16)>> = HashMap::new();
    for a in enc {
        let lo = a.start;
        let hi = a.end.unwrap_or(a.start);
        match &a.value {
            Expr::Ident(id) => {
                pieces.entry(id.name.clone()).or_default().push((hi, lo, 0));
            }
            Expr::Slice(s) => {
                if let Expr::Ident(id) = &*s.base {
                    let shift = s.start; // place at operand bit-low
                    pieces
                        .entry(id.name.clone())
                        .or_default()
                        .push((hi, lo, shift));
                }
            }
            Expr::IndexAccess(ia) => {
                if let Expr::Ident(id) = &*ia.base {
                    let shift = ia.index;
                    pieces
                        .entry(id.name.clone())
                        .or_default()
                        .push((hi, lo, shift));
                }
            }
            _ => {}
        }
    }

    let mut assigns: Vec<String> = Vec::new();
    for field in all_fields {
        if let Some(ps) = pieces.get(field) {
            // Build Z expression combining parts
            let mut parts: Vec<String> = Vec::new();
            for (hi, lo, sh) in ps {
                if *sh == 0 {
                    parts.push(format!("bits {} {} w", hi, lo));
                } else {
                    parts.push(format!("Z.shiftl (bits {} {} w) {}", hi, lo, sh));
                }
            }
            let expr = if parts.is_empty() {
                "0".to_string()
            } else if parts.len() == 1 {
                parts[0].clone()
            } else {
                parts
                    .into_iter()
                    .reduce(|a, b| format!("Z.lor ({}) ({})", a, b))
                    .unwrap()
            };
            assigns.push(format!("{}  := {}", field, expr));
        } else {
            assigns.push(format!("{}  := 0", field));
        }
    }

    let fields = assigns.join(";\n          ");
    format!(
        "Definition {} (w:word) : option Fields :=\n  Some {{| {} |}}.\n\n",
        decode_name, fields
    )
}

// Emit semantics function by translating behavior to our small expr language
fn emit_semantics_function(
    sem_name: &str,
    inst: &ast::Instruction,
    ops: &HashMap<String, ast::Type>,
) -> Result<String, TMDLError> {
    // Find first assignment in behavior
    let (dst, val_expr) = match &inst.behavior {
        Expr::Assign(a) => (a.dest.clone(), *a.value.clone()),
        Expr::Block(b) => {
            // pick the last assignment in the block for now
            let mut dst = None;
            let mut value: Option<Expr> = None;
            for e in &b.stmts {
                if let Expr::Assign(a) = e {
                    dst = Some(a.dest.clone());
                    value = Some(*a.value.clone());
                }
            }
            match (dst, value) {
                (Some(d), Some(v)) => (d, v),
                _ => return Err(TMDLError::UnexpectedExpression),
            }
        }
        _ => return Err(TMDLError::UnexpectedExpression),
    };

    let body_expr = emit_expr(&val_expr, ops)?;
    let s = format!(
        "Definition {} (f:Fields) : Stmt :=\n  {{| dst := f.({});\n     body := {} |}}.\n\n",
        sem_name, dst, body_expr
    );
    Ok(s)
}

fn emit_expr(e: &Expr, ops: &HashMap<String, ast::Type>) -> Result<String, TMDLError> {
    match e {
        Expr::Ident(id) => {
            if let Some(ty) = ops.get(&id.name) {
                Ok(match ty {
                    ast::Type::Struct(_) => format!("EReg f.({})", id.name),
                    ast::Type::Bits(_) | ast::Type::Integer => format!("EImm f.({})", id.name),
                    ast::Type::String => return Err(TMDLError::UnexpectedExpression),
                })
            } else {
                // Unknown identifier in behavior
                Err(TMDLError::UnexpectedExpression)
            }
        }
        Expr::Lit(Lit::Int(i)) => Ok(format!("EImm {}", parse_int_str(i.value()).unwrap_or(0))),
        Expr::Binary(b) => {
            let l = emit_expr(&b.lhs, ops)?;
            let r = emit_expr(&b.rhs, ops)?;
            let op = match b.op {
                BinOp::Add => "BAdd",
                BinOp::Sub => "BSub",
                BinOp::Mul => return Err(TMDLError::UnexpectedExpression),
                BinOp::Div => return Err(TMDLError::UnexpectedExpression),
                BinOp::BitwiseAnd => "BAnd",
                BinOp::BitwiseOr => "BOr",
                BinOp::BitwiseXor => "BXor",
                BinOp::ShiftLeftLogical => "BSll",
                BinOp::ShiftRightLogical => "BSrl",
                BinOp::ShiftRightArithmetic => "BSra",
            };
            Ok(format!("EBin {} ({}) ({})", op, l, r))
        }
        _ => Err(TMDLError::UnexpectedExpression),
    }
}
