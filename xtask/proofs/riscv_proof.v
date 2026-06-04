Require Import ZArith Lia.
Require Import SailStdpp.Base.
From Riscv Require Import rv64d rv64d_types.
Require Import riscv.

Definition word_equiv {n : nat} (wt : tmdl_word n) (ws : mword (Z.of_nat n)) : Prop :=
  tmdl_word_val wt = uint ws.

Section BridgeProofs.
  (* Helper: Simple math fact required by Sail *)
  Fact nat_to_Z_nonneg : forall n, Z.of_nat n >= 0.
  Proof. lia. Qed.

  Definition sail_to_tmdl {n : nat} (w : mword (Z.of_nat n)) : tmdl_word n.
  Proof.
    set (z := uint w).

    refine {| tmdl_word_val := z; tmdl_word_range := _ |}.

    pose proof (uint_range w (nat_to_Z_nonneg n)) as H_range.

    unfold tmdl_modulus.
    unfold z.

    destruct H_range as [H_zero H_upper].
    split.
    - apply H_zero.
    - (* Use 'lia' to solve the off-by-one arithmetic automatically *)
      lia.
  Defined.
End BridgeProofs.
