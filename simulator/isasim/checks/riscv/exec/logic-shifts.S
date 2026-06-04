# RUN: isasim --march=riscv64 --entry=_start --until-pc=done --trace-registers-end %s | filecheck %s

# Logic and shift correctness: 0xc & 0xa = 0x8, 0xc | 0xa = 0xe, 0xc ^ 0xa = 0x6,
# 1 << 4 = 0x10, 0x10 >> 1 = 0x8.
.global done
done:
  add x0, x0, x0
.global _start
_start:
  addi x1, x0, 12
  addi x2, x0, 10
  and  x3, x1, x2
  or   x4, x1, x2
  xor  x5, x1, x2
  addi x6, x0, 1
  slli x7, x6, 4
  srli x8, x7, 1

# CHECK-DAG: GPR[3] = 0x8
# CHECK-DAG: GPR[4] = 0xe
# CHECK-DAG: GPR[5] = 0x6
# CHECK-DAG: GPR[7] = 0x10
# CHECK-DAG: GPR[8] = 0x8
