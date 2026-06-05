# RUN: tir mc --march=rv32i --stage=regalloc %s | filecheck %s

.global foo
foo:
    addi x1, x0, 5
    addi x2, x0, 10
    add  x3, x1, x2
    sub  x4, x2, x1

# CHECK:      riscv.addi {rd = x1, rs1 = x0, imm = 5}
# CHECK-NEXT: riscv.addi {rd = x2, rs1 = x0, imm = 10}
# CHECK-NEXT: riscv.add {rd = x3, rs1 = x1, rs2 = x2}
# CHECK-NEXT: riscv.sub {rd = x4, rs1 = x2, rs2 = x1}
