(* Generic Rocq proof skeleton relating TMDL-generated Coq to Sail Rocq. *)

(* Sail-generated Rocq imports. Adjust the Require paths to your build setup. *)
Require Import SailStdpp.Base.
Require Import SailStdpp.Real.
Require Import SailStdpp.ConcurrencyInterfaceTypes.
Require Import SailStdpp.ConcurrencyInterface.
Require Import SailStdpp.ConcurrencyInterfaceBuiltins.
Require Import Riscv.rv64d_types.
Require Import Riscv.rv64d. (* if available; otherwise keep specific Requires as needed *)
Import Defs.

(* TMDL-generated Rocq module should provide these (names per your example): *)
(*
  - [Record TMDLState := { gpr : nat -> BitVec 64; pc : BitVec 64 }]
  - [Inductive TMDLInstr := ...]
  - [Definition encode_riscv : TMDLInstr -> BitVec 32]
  - [Definition execute_riscv : TMDLState -> TMDLInstr -> TMDLState]
  - [Definition read_gpr : TMDLState -> nat -> BitVec 64]
*)

(* If your generator uses BitVec, provide/coerce to Sail's [mword]. *)
Parameter BitVec : Z -> Type.
Parameter bitvec_to_mword : forall n, BitVec n -> mword n.
Parameter TMDLState : Type.
Parameter TMDLInstr : Type.
Parameter encode_riscv : TMDLInstr -> BitVec 32.
Parameter execute_riscv : TMDLState -> TMDLInstr -> TMDLState.
Parameter read_gpr : TMDLState -> nat -> BitVec 64.
Parameter pc : TMDLState -> BitVec 64.

(* An abstract Sail state type and evaluation function for the Sail M monad. *)
Parameter SailState : Type.
Parameter runM : forall {A : Type}, M A -> SailState -> (A * SailState).

(* Monad running laws sufficient for this development. *)
Axiom runM_return : forall (A:Type) (a:A) (σ:SailState), runM (returnM a) σ = (a, σ).
Axiom runM_bind   : forall (A B:Type) (ma:M A) (k:A -> M B) (σ:SailState),
  let '(a, σ') := runM ma σ in
  runM (ma >>= k) σ = runM (k a) σ'.

(* Observational equivalence between your state and Sail state. *)
Definition states_equiv (riscv_st : TMDLState) (sail_st : SailState) : Prop :=
  (forall (r:nat), r < 32 ->
    bitvec_to_mword 64 (read_gpr riscv_st r) =
      let '(v, _) := runM (rX_bits (Regidx (to_bits 5 (Z.of_nat r)))) sail_st in v
  ) /
  let '(pcv, _) := runM ((read_reg PC) : M (mword 64)) sail_st in
    bitvec_to_mword 64 (pc riscv_st) = pcv.

(* 1) Decode existence for our encoding. *)
Definition decode_of_encode (i : TMDLInstr) (σ : SailState) : Prop :=
  exists (sail_inst : instruction) (σ' : SailState),
    runM (encdec_backwards (bitvec_to_mword 32 (encode_riscv i))) σ = (sail_inst, σ').

(* 2) Refinement from decoder result through execution, matching observations. *)
Definition exec_refines
  (i : TMDLInstr)
  (sail_inst : instruction)
  (rσ : TMDLState)
  (sσ sσ' : SailState)
  : Prop :=
  runM (encdec_backwards (bitvec_to_mword 32 (encode_riscv i))) sσ = (sail_inst, sσ') ->
  states_equiv rσ sσ ->
  exists (res : ExecutionResult) (sσf : SailState),
    runM (execute sail_inst) sσ' = (res, sσf) /
    states_equiv (execute_riscv rσ i) sσf.

(* These two facts are the only per-instruction bridges needed. They can be
   proved once-and-for-all by automation over Sail’s encoders/decoders and your
   generator’s encoders/behavior, and will continue to hold as new instructions
   are added because they quantify over [i : TMDLInstr]. *)
Hypothesis decode_of_encode_exists : forall (i : TMDLInstr) (σ : SailState), decode_of_encode i σ.
Hypothesis exec_refines_from_decode :
  forall (i : TMDLInstr) (sail_inst : instruction) (rσ : TMDLState) (sσ sσ' : SailState),
    exec_refines i sail_inst rσ sσ sσ'.

(* Master theorem: observational equivalence of one-step executions for any instruction. *)
Theorem observational_equivalence (instr : TMDLInstr) :
  forall (riscv_st : TMDLState) (sail_st : SailState),
    states_equiv riscv_st sail_st ->
    (* 1) Sail decodes our encoding *)
    exists (sail_inst : instruction) (sail_st' : SailState),
      runM (encdec_backwards (bitvec_to_mword 32 (encode_riscv instr))) sail_st = (sail_inst, sail_st') /
    (* 2) Sail executes it, producing some result and final state *)
    exists (sail_st_final : SailState) (res : ExecutionResult),
      runM (execute sail_inst) sail_st' = (res, sail_st_final) /
    (* 3) Observations match *)
      states_equiv (execute_riscv riscv_st instr) sail_st_final.
Proof.
  intros rσ sσ Hrel.
  destruct (decode_of_encode_exists instr sσ) as [sInst [sσ' Hdec]].
  specialize (exec_refines_from_decode instr sInst rσ sσ sσ').
  unfold exec_refines in exec_refines_from_decode.
  specialize (exec_refines_from_decode Hdec Hrel).
  destruct exec_refines_from_decode as [res [sσf [Hexec Hrel']]].
  eexists; eexists; split; [exact Hdec|].
  eexists; eexists; split; [exact Hexec|exact Hrel'].
Qed.

(* Optional: a simple encoding example lemma can be stated and proved by reflexivity
   once [encode_riscv] expands to a closed expression for concrete operands. *)
(* Example: ADD x3, x1, x2 *)
(* Lemma encode_add_example :
  encode_riscv (ADD 3 1 2) =
    ((('b"0000000") : mword 7) ++
     (to_bits 5 2) ++
     (to_bits 5 1) ++
     (('b"000") : mword 3) ++
     (to_bits 5 3) ++
     (('b"0110011") : mword 7)).
Proof. reflexivity. Qed. *)
