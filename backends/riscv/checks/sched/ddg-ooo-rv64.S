# RUN: tir sched --march=rv64i --model=ooo %s | filecheck %s

# RAW edge latency comes from the selected performance model: the OutOfOrderCore
# overrides `add` to 2 cycles, so the dependent `sub` waits lat=2 on it.

.global f
f:
    addi x1, x0, 5
    addi x2, x0, 9
    add  x3, x1, x2
    sub  x4, x3, x1

# CHECK:      i2  riscv.add {rd = x3, rs1 = x1, rs2 = x2}  ; deps: raw i0 GPR[1] lat=1, raw i1 GPR[2] lat=1
# CHECK-NEXT:   i3  riscv.sub {rd = x4, rs1 = x3, rs2 = x1}  ; deps: raw i2 GPR[3] lat=2, raw i0 GPR[1] lat=1
