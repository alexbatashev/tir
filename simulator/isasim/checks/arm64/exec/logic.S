# RUN: isasim --march=arm64 --entry=_start --until-pc=done --trace-registers-end %s | filecheck %s

# AArch64 logic correctness: 0xc & 0xa = 0x8, 0xc | 0xa = 0xe (orr),
# 0xc ^ 0xa = 0x6 (eor).
.global done
done:
  add x0, x0, x0
.global _start
_start:
  add x1, x0, 12
  add x2, x0, 10
  and x3, x1, x2
  orr x4, x1, x2
  eor x5, x1, x2

# CHECK-DAG: GPR[3] = 0x8
# CHECK-DAG: GPR[4] = 0xe
# CHECK-DAG: GPR[5] = 0x6
