# RUN: isasim --march=arm64 --entry=_start --until-pc=done --trace-registers-end %s | filecheck %s

# Execution correctness across the GPR/GPRsp alias: values set with immediate
# `add` (GPRsp) are read back by register `add`/`sub` (GPR). 5 + 7 = 12 (0xc),
# 12 - 5 = 7.
.global done
done:
  add x0, x0, x0
.global _start
_start:
  add x1, x0, 5
  add x2, x0, 7
  add x3, x1, x2
  sub x4, x3, x1

# CHECK: final registers:
# CHECK-DAG: GPR[1] = 0x5
# CHECK-DAG: GPR[2] = 0x7
# CHECK-DAG: GPR[3] = 0xc
# CHECK-DAG: GPR[4] = 0x7
