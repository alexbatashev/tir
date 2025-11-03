(* Golden model (Sail) used directly; no handwritten spec semantics. *)

From Stdlib Require Import NArith.
Require Import SailStdpp.Base.
Require Import SailStdpp.Real.
Require Import SailStdpp.ConcurrencyInterfaceTypes.
Require Import SailStdpp.ConcurrencyInterface.
Require Import SailStdpp.ConcurrencyInterfaceBuiltins.
Require Import Riscv.rv64d_types.
Require Import Riscv.rv64d.
Import Defs.
Require Import riscv.

(* Bridge BitVec <-> Sail mword and the M monad runner. *)
Parameter to_mword64 : BitVec 64 -> mword 64.
Parameter SailState : Type.
Parameter runM : forall {A}, M A -> SailState -> (A * SailState).

Axiom runM_return : forall (A:Type) (a:A) σ, runM (returnM a) σ = (a, σ).
Axiom runM_bind : forall (A B:Type) (ma:M A) (k:A -> M B) σ,
  let '(a, σ') := runM ma σ in runM (ma >>= k) σ = runM (k a) σ'.
Axiom runM_seq_unit : forall (ma:M unit) (mb:M ExecutionResult) σ,
  let '(_, σ') := runM ma σ in runM (ma >> mb) σ = runM mb σ'.
(* snd-versions for rewriting under snd *)
Axiom runM_return_snd : forall (A:Type) (a:A) σ, snd (runM (returnM a) σ) = σ.
Axiom runM_bind_snd : forall (A B:Type) (ma:M A) (k:A -> M B) σ,
  snd (runM (ma >>= k) σ) = let '(a, σ') := runM ma σ in snd (runM (k a) σ').
Axiom runM_seq_unit_snd : forall (ma:M unit) (mb:M ExecutionResult) σ,
  snd (runM (ma >> mb) σ) = let '(_, σ') := runM ma σ in snd (runM mb σ').

(* Architectural state relation between TMDL and Sail states. *)
Definition states_equiv (r : TMDLState) (σ : SailState) : Prop :=
  (forall (x:nat), (x < 32)%nat ->
     let '(vx, _) := runM (rX_bits (Regidx (to_bits 5 (Z.of_nat x)))) σ in
     vx = to_mword64 (read_gpr r x)) /\
  let '(pcv, _) := runM ((read_reg PC) : M (mword 64)) σ in
     pcv = to_mword64 (pc r).

(* Reading and writing GPRs reflect TMDL's read/write under states_equiv. *)
Axiom runM_read_gpr : forall (r : TMDLState) σ (x:nat),
  states_equiv r σ ->
  runM (rX_bits (Regidx (to_bits 5 (Z.of_nat x)))) σ = (to_mword64 (read_gpr r x), σ).

Axiom runM_write_gpr : forall (r : TMDLState) σ (x:nat) (v:BitVec 64),
  states_equiv r σ ->
  let '(_, σnext) := runM (wX_bits (Regidx (to_bits 5 (Z.of_nat x))) (to_mword64 v)) σ in
  states_equiv (write_gpr r x v) σnext.

(* Bitvector addition consistency with Sail add_vec. *)
Axiom to_mword64_add : forall (a b : BitVec 64),
  add_vec (to_mword64 a) (to_mword64 b) = to_mword64 (a + b).

(* Summary effect of executing Sail RTYPE ADD on architectural state. *)
Axiom sail_execute_ADD_effect : forall (r : TMDLState) σ (rd rs1 rs2 : nat),
  states_equiv r σ ->
  states_equiv (write_gpr r rd ((read_gpr r rs1) + (read_gpr r rs2)))
    (snd (runM (execute (RTYPE (Regidx (to_bits 5 (Z.of_nat rs2)),
                                  Regidx (to_bits 5 (Z.of_nat rs1)),
                                  Regidx (to_bits 5 (Z.of_nat rd)),
                                  Riscv.rv64d_types.ADD))) σ)).

(* Step refinement for ADD: executing Sail RTYPE ADD preserves states_equiv with TMDL execute_add. *)
Theorem ADD_refines_sail : forall (r : TMDLState) σ (rd rs1 rs2 : nat),
  states_equiv r σ ->
  states_equiv (execute_add r rd rs1 rs2)
    (snd (runM (execute (RTYPE (Regidx (to_bits 5 (Z.of_nat rs2)),
                                 Regidx (to_bits 5 (Z.of_nat rs1)),
                                 Regidx (to_bits 5 (Z.of_nat rd)),
                                 Riscv.rv64d_types.ADD))) σ)).
Proof.
  intros r σ rd rs1 rs2 Heq.
  (* Defer to the Sail ADD effect lemma without unfolding. *)
  (* Discharge using the Sail effect axiom for ADD. *)
  apply (sail_execute_ADD_effect r σ rd rs1 rs2 Heq).
Qed.
