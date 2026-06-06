# RUN: tir sched --march=rv32i %s | filecheck %s

# Register dependencies reconstructed from physical-register form: RAW (true),
# WAR (anti) and WAW (output). Reusing x1 forces anti/output edges.

.global f
f:
    addi x1, x0, 5
    add  x2, x1, x1
    addi x1, x0, 9
    add  x3, x1, x2

# CHECK:      sched @f {
# CHECK-NEXT:   i0  riscv.addi {rd = x1, rs1 = x0, imm = 5}
# CHECK-NEXT:   i1  riscv.add {rd = x2, rs1 = x1, rs2 = x1}  ; deps: raw i0 GPR[1] lat=1
# CHECK-NEXT:   i2  riscv.addi {rd = x1, rs1 = x0, imm = 9}  ; deps: waw i0 GPR[1] lat=0, war i1 GPR[1] lat=0
# CHECK-NEXT:   i3  riscv.add {rd = x3, rs1 = x1, rs2 = x2}  ; deps: raw i2 GPR[1] lat=1, raw i1 GPR[2] lat=1
# CHECK-NEXT: }
