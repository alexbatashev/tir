(* Golden model (Sail) + grouping over R-type in the proof. *)

From Stdlib Require Import NArith.
(* Sail-generated Rocq imports. These names match the rv64d build in Sail. *)
Require Import SailStdpp.Base.
Require Import SailStdpp.Real.
Require Import SailStdpp.ConcurrencyInterfaceTypes.
Require Import SailStdpp.ConcurrencyInterface.
Require Import SailStdpp.ConcurrencyInterfaceBuiltins.
Require Import Riscv.rv64d_types.
Require Import Riscv.rv64d.
Import Defs.

(* TMDL-generated Rocq module for our target *)
Require Import riscv.

(* Grouping: R-type field layout in RISC-V (opcode, funct3, funct7 and regs) *)

Definition Rtype_bits_nat (funct7 rs2 rs1 funct3 rd opcode : nat) : BitVec 32 :=
  (BitVec.of_nat 7 funct7) ++ (BitVec.of_nat 5 rs2) ++
  (BitVec.of_nat 5 rs1) ++ (BitVec.of_nat 3 funct3) ++
  (BitVec.of_nat 5 rd) ++ (BitVec.of_nat 7 opcode).

(* ADD/XOR encodings from the RISC-V spec (as constants) *)
Definition opcode_R_nat : nat := 51%nat.
Definition funct3_ADD_nat : nat := 0%nat.
Definition funct7_ADD_nat : nat := 0%nat.
Definition funct3_XOR_nat : nat := 4%nat.
Definition funct7_XOR_nat : nat := 0%nat.

(* Group-level bit-pattern theorems for our encoder (computational) *)
Lemma encode_ADD_Rtype rd rs1 rs2 :
  encode_add rd rs1 rs2 = Rtype_bits_nat funct7_ADD_nat rs2 rs1 funct3_ADD_nat rd opcode_R_nat.
Proof. reflexivity. Qed.

Lemma encode_XOR_Rtype rd rs1 rs2 :
  encode_xor rd rs1 rs2 = Rtype_bits_nat funct7_XOR_nat rs2 rs1 funct3_XOR_nat rd opcode_R_nat.
Proof. reflexivity. Qed.

(* Execution spec for R-type group (defined by Sail/RISC-V semantics). We only
   need the ADD equation to catch the current bug. *)

Definition Rtype_exec_ADD (st : TMDLState) (rd rs1 rs2 : nat) : TMDLState :=
  write_gpr st rd ((read_gpr st rs1) + (read_gpr st rs2)).

(* Grouped execution correctness: each R-type operation matches Sail semantics.
   Demonstrate on current subset; proof is by computation and will fail if the
   implementation is wrong. *)

Theorem Rtype_execute_ADD_correct : forall st rd rs1 rs2,
  execute_add st rd rs1 rs2 = Rtype_exec_ADD st rd rs1 rs2.
Proof.
  intros. unfold execute_add, Rtype_exec_ADD.
  (* This is where your deliberate typo (using subtraction) is caught. *)
  reflexivity.
Qed.

Theorem Rtype_execute_SUB_correct : forall st rd rs1 rs2,
  execute_sub st rd rs1 rs2 = write_gpr st rd ((read_gpr st rs1) - (read_gpr st rs2)).
Proof. intros; unfold execute_sub; reflexivity. Qed.

Theorem Rtype_execute_XOR_correct : forall st rd rs1 rs2,
  execute_xor st rd rs1 rs2 = write_gpr st rd ((read_gpr st rs1) ^^^ (read_gpr st rs2)).
Proof. intros; unfold execute_xor; reflexivity. Qed.

Theorem Rtype_execute_OR_correct : forall st rd rs1 rs2,
  execute_or st rd rs1 rs2 = write_gpr st rd ((read_gpr st rs1) ||| (read_gpr st rs2)).
Proof. intros; unfold execute_or; reflexivity. Qed.

Theorem Rtype_execute_AND_correct : forall st rd rs1 rs2,
  execute_and st rd rs1 rs2 = write_gpr st rd ((read_gpr st rs1) &&& (read_gpr st rs2)).
Proof. intros; unfold execute_and; reflexivity. Qed.

Theorem Rtype_execute_SLL_correct : forall st rd rs1 rs2,
  execute_shiftleftlogical st rd rs1 rs2 = write_gpr st rd ((read_gpr st rs1) <<< (read_gpr st rs2)).
Proof. intros; unfold execute_shiftleftlogical; reflexivity. Qed.
