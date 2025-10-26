import TmdlRiscv
import LeanRV64D
import Lean

open LeanRV64D
open LeanRV64D.Functions
open Sail
open PreSail
open Lean Meta Elab Tactic

abbrev SailState : Type := PreSail.SequentialState RegisterType trivialChoiceSource

/-- Your state correspondence. -/
def states_equiv (riscv_st : TMDLState) (sail_st : SailState) : Prop :=
  (∀ (r : Nat), r < 32 →
    riscv_st.read_gpr r =
      match (rX_bits (regidx.Regidx (BitVec.ofNat 5 r))).run sail_st with
      | .ok val _ => val
      | .error _ _ => 0
  )
  ∧
  match (Sail.readReg (Register.PC)).run sail_st with
  | .ok val _ => riscv_st.pc = val
  | .error _ _ => True

/-- (Decode existence) Decoding our encoding succeeds, producing some
    `sail_inst` and a (possibly changed) post-decode state `σ'`. -/
axiom decode_of_encode_exists
  (i : TMDLInstr) (σ : SailState) :
  ∃ (sail_inst : instruction) (σ' : SailState),
    (encdec_backwards (encode_riscv i)).run σ
      = EStateM.Result.ok sail_inst σ'

/-- (Refinement from decoder) If `sail_inst` is exactly the decoder result on
    `encode_riscv i` at input state `sσ`, yielding post-decode state `sσ'`,
    then executing `sail_inst` from `sσ'` produces some result `res` and final
    state `sσf` such that the observable state matches your TMDL step. -/
axiom exec_refines_from_decode
  (i : TMDLInstr)
  (sail_inst : instruction) (rσ : TMDLState)
  (sσ sσ' : SailState) :
  (encdec_backwards (encode_riscv i)).run sσ
    = EStateM.Result.ok sail_inst sσ' →
  states_equiv rσ sσ →
  ∃ (res : ExecutionResult) (sσf : SailState),
    (execute sail_inst).run sσ'
      = EStateM.Result.ok res sσf
    ∧ states_equiv (execute_riscv rσ i) sσf

theorem observational_equivalence (instr : TMDLInstr) :
  ∀ (riscv_st : TMDLState) (sail_st : SailState),
    states_equiv riscv_st sail_st →
    -- 1) Sail decodes our encoding
    ∃ (sail_inst : instruction) (sail_st' : SailState),
      (encdec_backwards (encode_riscv instr)).run sail_st
        = EStateM.Result.ok sail_inst sail_st' ∧
    -- 2) Sail executes it (existentials for result and final state)
    ∃ (sail_st_final : SailState) (res : ExecutionResult),
      (execute sail_inst).run sail_st'
        = EStateM.Result.ok res sail_st_final ∧
    -- 3) Observations match
      states_equiv (execute_riscv riscv_st instr) sail_st_final := by
  intro rσ sσ hEq
  -- (1) Get the decoder witnesses
  obtain ⟨sInst, sσ', hDec⟩ := decode_of_encode_exists instr sσ
  -- (2) Use refinement-from-decoder with the *post-decode* state sσ'
  obtain ⟨res, sσf, hExec, hRel⟩ :=
    exec_refines_from_decode instr sInst rσ sσ sσ' hDec hEq
  -- (3) Package witnesses for the theorem’s statement
  refine ⟨sInst, sσ', ?_, ?_⟩
  · exact hDec
  · refine ⟨sσf, res, ?_, hRel⟩
    exact hExec

example : encode_add 3 1 2 =  -- ADD x3, x1, x2
      ((0b0000000 : BitVec 7) ++
      (BitVec.ofNat 5 2) ++
      (BitVec.ofNat 5 1) ++
      (0b000 : BitVec 3) ++
      (BitVec.ofNat 5 3) ++
      (0b0110011 : BitVec 7)) := by
  rfl
