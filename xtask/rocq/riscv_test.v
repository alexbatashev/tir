(* Self-contained snippet equivalence check between TMDL and the Sail model. *)

From Stdlib Require Import ZArith.
Require Import SailStdpp.Base.
Require Import SailStdpp.Real.
Require Import SailStdpp.ConcurrencyInterfaceTypes.
Require Import SailStdpp.ConcurrencyInterface.
Require Import SailStdpp.ConcurrencyInterfaceBuiltins.
Require Import Riscv.rv64d_types.
Require Import Riscv.rv64d.
Import Defs.
Require Import riscv.

Parameter to_mword64 : BitVec 64 -> mword 64.
Parameter SailState : Type.
Parameter runM : forall {A}, M A -> SailState -> (A * SailState).

Axiom runM_return : forall (A:Type) (a:A) σ, runM (returnM a) σ = (a, σ).
Axiom runM_bind : forall (A B:Type) (ma:M A) (k:A -> M B) σ,
  let '(a, σ') := runM ma σ in runM (ma >>= k) σ = runM (k a) σ'.
Axiom runM_seq_unit : forall (ma:M unit) (mb:M ExecutionResult) σ,
  let '(_, σ') := runM ma σ in runM (ma >> mb) σ = runM mb σ'.
Axiom runM_return_snd : forall (A:Type) (a:A) σ, snd (runM (returnM a) σ) = σ.
Axiom runM_bind_snd : forall (A B:Type) (ma:M A) (k:A -> M B) σ,
  snd (runM (ma >>= k) σ) = let '(a, σ') := runM ma σ in snd (runM (k a) σ').
Axiom runM_seq_unit_snd : forall (ma:M unit) (mb:M ExecutionResult) σ,
  snd (runM (ma >> mb) σ) = let '(_, σ') := runM ma σ in snd (runM mb σ').

Definition states_equiv (r : TMDLState) (σ : SailState) : Prop :=
  (forall (x:nat), (x < 32)%nat ->
     let '(vx, _) := runM (rX_bits (Regidx (to_bits 5 (Z.of_nat x)))) σ in
     vx = to_mword64 (read_gpr r x)) /\
  let '(pcv, _) := runM ((read_reg PC) : M (mword 64)) σ in
     pcv = to_mword64 (pc r).

Axiom runM_read_gpr : forall (r : TMDLState) σ (x:nat),
  states_equiv r σ ->
  runM (rX_bits (Regidx (to_bits 5 (Z.of_nat x)))) σ = (to_mword64 (read_gpr r x), σ).

Axiom runM_write_gpr : forall (r : TMDLState) σ (x:nat) (v:BitVec 64),
  states_equiv r σ ->
  let '(_, σnext) := runM (wX_bits (Regidx (to_bits 5 (Z.of_nat x))) (to_mword64 v)) σ in
  states_equiv (write_gpr r x v) σnext.

Axiom to_mword64_add : forall (a b : BitVec 64),
  add_vec (to_mword64 a) (to_mword64 b) = to_mword64 (a + b).

Axiom sail_execute_ADD_effect : forall (r : TMDLState) σ (rd rs1 rs2 : nat),
  states_equiv r σ ->
  states_equiv (write_gpr r rd ((read_gpr r rs1) + (read_gpr r rs2)))
    (snd (runM (execute (RTYPE (Regidx (to_bits 5 (Z.of_nat rs2)),
                                  Regidx (to_bits 5 (Z.of_nat rs1)),
                                  Regidx (to_bits 5 (Z.of_nat rd)),
                                  Riscv.rv64d_types.ADD))) σ)).

Definition sail_add_instr (rd rs1 rs2 : nat) :=
  RTYPE (Regidx (to_bits 5 (Z.of_nat rs2)),
         Regidx (to_bits 5 (Z.of_nat rs1)),
         Regidx (to_bits 5 (Z.of_nat rd)),
         Riscv.rv64d_types.ADD).

Lemma add_then_add_states_equiv :
  forall (r : TMDLState) σ
         (rd1 rs1a rs1b rd2 rs2a rs2b : nat),
    states_equiv r σ ->
    states_equiv
      (execute_riscv (execute_riscv r (ADD rd1 rs1a rs1b))
        (ADD rd2 rs2a rs2b))
      (snd (runM (execute (sail_add_instr rd2 rs2a rs2b))
        (snd (runM (execute (sail_add_instr rd1 rs1a rs1b)) σ)))).
Proof.
  intros r σ rd1 rs1a rs1b rd2 rs2a rs2b Heq.
  set (r1 := execute_riscv r (ADD rd1 rs1a rs1b)).
  set (σ1 := snd (runM (execute (sail_add_instr rd1 rs1a rs1b)) σ)).
  assert (states_equiv r1 σ1) as Hstep1.
  { unfold r1, σ1. simpl.
    apply sail_execute_ADD_effect; assumption. }
  set (r2 := execute_riscv r1 (ADD rd2 rs2a rs2b)).
  set (σ2 := snd (runM (execute (sail_add_instr rd2 rs2a rs2b)) σ1)).
  assert (states_equiv r2 σ2) as Hstep2.
  { unfold r2, σ2. simpl.
    apply sail_execute_ADD_effect; assumption. }
  unfold r2, σ2 in Hstep2.
  now simpl in Hstep2.
Qed.

Corollary add_sequence_example :
  forall (r : TMDLState) σ,
    states_equiv r σ ->
    states_equiv
      (execute_riscv (execute_riscv r (ADD 3 1 2)) (ADD 5 3 1))
      (snd (runM (execute (sail_add_instr 5 3 1))
        (snd (runM (execute (sail_add_instr 3 1 2)) σ)))).
Proof.
  intros r σ Heq.
  eapply add_then_add_states_equiv; eauto.
Qed.

Lemma add_then_add_readback :
  forall (r : TMDLState) σ
         (rd1 rs1a rs1b rd2 rs2a rs2b : nat),
    states_equiv r σ ->
    let final_state :=
        execute_riscv (execute_riscv r (ADD rd1 rs1a rs1b))
          (ADD rd2 rs2a rs2b) in
    let final_sigma :=
        snd (runM (execute (sail_add_instr rd2 rs2a rs2b))
          (snd (runM (execute (sail_add_instr rd1 rs1a rs1b)) σ))) in
    runM (rX_bits (Regidx (to_bits 5 (Z.of_nat rd2)))) final_sigma =
      (to_mword64 (read_gpr final_state rd2), final_sigma).
Proof.
  intros r σ rd1 rs1a rs1b rd2 rs2a rs2b Heq final_state final_sigma.
  assert (states_equiv final_state final_sigma) as Hfinal.
  { subst final_state final_sigma.
    eapply add_then_add_states_equiv; eauto. }
  apply runM_read_gpr; assumption.
Qed.
