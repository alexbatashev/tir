# RUN: isasim --march=riscv64 --entry=_start --until-pc=done --trace-instructions %s | filecheck %s

# Parser/decoder coverage: the assembler round-trips each instruction into the
# machine dialect, and the instruction trace prints the decoded operands. The
# `done` sentinel is declared first so it lands after `_start` in memory, giving
# the body a fall-through to stop on.
.global done
done:
  add x0, x0, x0
.global _start
_start:
  addi x1, x0, 5
  addi x2, x0, 7
  add  x3, x1, x2

# CHECK: addi rd=GPR[1], rs1=GPR[0], imm=5
# CHECK-NEXT: addi rd=GPR[2], rs1=GPR[0], imm=7
# CHECK-NEXT: add rd=GPR[3], rs1=GPR[1], rs2=GPR[2]
