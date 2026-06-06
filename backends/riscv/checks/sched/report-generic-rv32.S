# RUN: tir sched --march=rv32i --iterations=100 %s | filecheck %s

# With no --model selected, scheduling falls back to a generic single-issue core
# that costs every instruction one cycle: dispatch width 1, latency/rthroughput 1
# for all ops, and (no functional units declared) no resource section.

.global f
f:
    addi x1, x0, 5
    add  x2, x1, x1
    addi x1, x0, 9
    add  x3, x1, x2

# CHECK:      Dispatch Width:    1
# CHECK:      [1]    [2]    Instructions:
# CHECK-NEXT: 1      1      addi GPR1 <- GPR0
# CHECK-NEXT: 1      1      add GPR2 <- GPR1, GPR1
# CHECK-NEXT: 1      1      addi GPR1 <- GPR0
# CHECK-NEXT: 1      1      add GPR3 <- GPR1, GPR2
