# RUN: tir sched --march=rv64i --model=rv64-ooo --iterations=100 %s | filecheck %s

# Per-instruction cost comes from the selected machine: the OutOfOrderCore is
# 4-wide and overrides `add` to latency 2, so it shows up in the instruction info
# table while the other ops keep the WriteIALU default of 1.

.global f
f:
    addi x1, x0, 5
    addi x2, x0, 9
    add  x3, x1, x2
    sub  x4, x3, x1

# CHECK:      Dispatch Width:    4
# CHECK:      [1]    [2]    Instructions:
# CHECK-NEXT: 1      1      addi GPR1 <- GPR0
# CHECK-NEXT: 1      1      addi GPR2 <- GPR0
# CHECK-NEXT: 2      1      add GPR3 <- GPR1, GPR2
# CHECK-NEXT: 1      1      sub GPR4 <- GPR3, GPR1
# CHECK:      Resources:
# CHECK-NEXT: [0]: ALU
# CHECK-NEXT: [1]: LSU
