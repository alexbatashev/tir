# TMDL → Rocq/Coq Code Generation (Phase 1)

This document explains how TMDL specifications map to generated Rocq/Coq definitions in Phase 1 (ISA definitions: encodings, decoders, small-step semantics). The generation is ISA‑agnostic and relies solely on the TMDL AST: instruction names, operands, encodings, and behavior are never hardcoded.

## Quick Example

TMDL (RISC‑V, R‑type):

```tmdl
template ALUOp for [RV32I, RV64I] : RType {
  param OPCODE: bits<7> = 0b0110011;
  param FUNCT7: bits<7> = 0b0000000;
}

instruction Add for [RV32I, RV64I] : ALUOp {
  param MNEMONIC: String = "add";
  param FUNCT3: bits<3> = 0b000;
  behavior { rd = rs1 + rs2; }
}

instruction ShiftRightArithmetic for [RV32I, RV64I] : ALUOp {
  param MNEMONIC: String = "sra";
  param FUNCT3: bits<3> = 0b101;
  param FUNCT7: bits<7> = 0b0100000;
  behavior { rd = rs1 >>> rs2; }
}
```

Generated (shape, simplified):

```coq
Definition ADD_mask : Z := (* mask for fixed fields *).
Definition ADD_pat  : Z := (* pattern for fixed fields *).

Definition decode_ADD (w:word) : option Fields :=
  Some {| rd := bits 11 7 w; rs1 := bits 19 15 w; rs2 := bits 24 20 w; imm := 0 |}.

Definition sem_ADD (f:Fields) : Stmt :=
  {| dst := f.(rd); body := EBin BAdd (EReg f.(rs1)) (EReg f.(rs2)) |}.

Definition ADD_desc : InstrDesc :=
  {| mask := ADD_mask; pat := ADD_pat; decode := decode_ADD; sem := sem_ADD; ilen := 4 |}.

Definition SHIFTRIGHTARITHMETIC_desc : InstrDesc :=
  {| mask := ...; pat := ...; decode := decode_SHIFTRIGHTARITHMETIC;
     sem := (fun f => {| dst := f.(rd); body := EBin BSra (EReg f.(rs1)) (EReg f.(rs2)) |});
     ilen := 4 |}.

Definition table : list InstrDesc := [ADD_desc; SHIFTRIGHTARITHMETIC_desc; ...].
```

## Generated Prelude (Core Model)

```coq
Definition word := Z.

Inductive binop := BAdd | BSub | BAnd | BOr | BXor | BSll | BSrl | BSra.
Inductive expr := EReg Z | EImm Z | EBin binop expr expr.
Record Stmt := { dst: Z; body: expr }.

Definition pow2 (w:Z) := 2 ^ w.
Definition maskw (w:Z) := pow2 w - 1.
Definition trunc (w z:Z) := Z.land z (maskw w).
Definition bits (hi lo : Z) (x : Z) := Z.land (Z.shiftr x lo) (maskw (hi - lo + 1)).

Module Params. Parameter XLEN : Z. Axiom XLEN_pos : 0 < XLEN. End Params.
Definition normalize z := trunc Params.XLEN z.
Definition shamt z := Z.land z (Params.XLEN - 1).

(* Arithmetic right shift (sign-filling) over XLEN-bit vectors *)
Definition sra_bv (w x sh:Z) : Z :=
  let v := normalize x in let sa := shamt sh in
  let srl := Z.shiftr v sa in
  let sign := Z.land v (Z.shiftl 1 (w - 1)) in
  if Z.eqb sign 0 then srl else Z.lor srl (Z.shiftl (maskw sa) (w - sa)).

(* Z-math (mask once at write) *)
Fixpoint eval_z s e := ...
Definition exec_stmt_z s st := let v := normalize (eval_z s st.(body)) in write_reg s st.(dst) v.

(* Bitvector style (mask per primitive) *)
Fixpoint eval_bv s e := ...
Definition exec_stmt_bv s st := write_reg s st.(dst) (eval_bv s st.(body)).

Definition next_pc_by s ilen := s.(pc) + ilen.
Definition step   tbl s iw := ... (* uses exec_stmt    , next_pc_by *)
Definition step_z tbl s iw := ... (* uses exec_stmt_z  , next_pc_by *)
Definition step_bv tbl s iw := ... (* uses exec_stmt_bv, next_pc_by *)
```

## Fields and Instruction Descriptor

```coq
Record Fields := { rd: Z; rs1: Z; rs2: Z; imm: Z (* union of operands *) }.

Record InstrDesc := {
  mask  : Z; pat : Z;
  decode : word -> option Fields;
  sem    : Fields -> Stmt;
  ilen   : Z (* bytes *)
}.

Fixpoint decode_table (ws:list InstrDesc) (w:word) : option (InstrDesc * Fields) := ...
```

## Mapping Rules (TMDL → Coq)

- Operands/params: collected along template ancestry (root → leaf), with instruction overrides last.
- Encoding → mask/pattern: constant/param arms set bits in `mask` and value in `pat`; operand arms remain unmasked.
- Decode: each operand gathers its bitpieces via `bits hi lo w` and ORs shifted pieces if discontiguous.
- Behavior: assignment to `rd` becomes `Stmt` with `dst := f.(rd)` and `body` mapped over binops.
  - Operators: `+ - | & ^ << >> >>>` map to `BAdd BSub BOr BAnd BXor BSll BSrl BSra`.

## Determinism and Overlap

```coq
Definition matches (d:InstrDesc) (w:word) : bool := Z.eqb (Z.land w d.(mask)) d.(pat).
Lemma matches_eq d w : matches d w = true <-> Z.land w d.(mask) = d.(pat).

Definition patterns_overlap d1 d2 : bool :=
  Z.eqb (Z.land d1.(pat) d2.(mask)) d2.(pat)
  &&   Z.eqb (Z.land d2.(pat) d1.(mask)) d1.(pat).

Definition nonoverlap_masks (ds:list InstrDesc) : bool :=
  forallb (fun i => forallb (fun j => if Nat.eqb i j then true
                           else negb (patterns_overlap (nth i ds dummy_desc)
                                                        (nth j ds dummy_desc)))
                                (seq 0 (length ds)))
          (seq 0 (length ds)).

Definition nonoverlap (ds:list InstrDesc) : Prop :=
  forall d1 d2 w, In d1 ds -> In d2 ds ->
    Z.land w d1.(mask) = d1.(pat) -> Z.land w d2.(mask) = d2.(pat) -> d1 = d2.
```

The generator emits `patterns_overlap_sound` and `nonoverlap_masks_sound` as admitted lemmas for you to prove once.

## Register Invariants and Frame Lemmas

```coq
Lemma write_reg_x0 s i v : (write_reg s i v).(rf) 0 = s.(rf) 0.
Lemma write_reg_other s i v j : j <> i -> (write_reg s i v).(rf) j = s.(rf) j.
Lemma step_preserves_x0   tbl s iw s' : s.(rf) 0 = 0 -> step   tbl s iw = Some s' -> s'.(rf) 0 = 0.
Lemma step_z_preserves_x0 tbl s iw s' : s.(rf) 0 = 0 -> step_z tbl s iw = Some s' -> s'.(rf) 0 = 0.
Lemma step_bv_preserves_x0 tbl s iw s' : s.(rf) 0 = 0 -> step_bv tbl s iw = Some s' -> s'.(rf) 0 = 0.
```

## Instruction Length

- `ilen` is inferred from encoding arms as `ceil((max_bit+1)/8)`. This supports 2‑byte compressed encodings alongside 4/8‑byte instructions. `step*` advance PC by `ilen` using `next_pc_by`.

## Choosing a Semantics for Proofs

- Use `step`/`step_z` for Z‑math (single truncation at write) — pleasant for users.
- Use `step_bv` for bit‑precise, per‑op masked semantics — convenient when matching Sail’s word ops.

## Truncation Lemmas

Basic algebraic facts used to move truncation across operations:

```coq
Lemma trunc_idem z : trunc Params.XLEN (trunc Params.XLEN z) = trunc Params.XLEN z.
Lemma trunc_add x y : trunc XLEN (trunc XLEN x + trunc XLEN y) = trunc XLEN (x + y).
Lemma trunc_and x y : trunc XLEN (Z.land (trunc XLEN x) (trunc XLEN y)) = trunc XLEN (Z.land x y).
(* similarly for or/xor/sll/srl/sra *)
```

## Singleton Decode Lemmas

For each instruction `FOO`:

```coq
Lemma decode_table_singleton_FOO w f :
  Z.land w FOO_desc.(mask) = FOO_desc.(pat) ->
  decode_FOO w = Some f ->
  decode_table [FOO_desc] w = Some (FOO_desc, f).
```

## Limitations & Roadmap

- Behavior translation currently supports assignments with binary ops and simple identifiers. `if`, function calls, and complex field/slice ops in behavior can be added as needed.
- Some general lemmas are emitted as `Admitted` to be proven once in your proof environment.

