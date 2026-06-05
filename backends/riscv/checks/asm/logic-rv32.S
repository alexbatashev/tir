# RUN: tir mc --march=rv32i --stage=regalloc %s | filecheck %s

    .section .text
    .global logic_test
logic_test:
    addi x1, x0, 0xF0
    srli x2, x1, 2
    slli x3, x1, 1
    or   x4, x2, x3
    and  x5, x3, x1
    xor  x6, x5, x4

# CHECK:      riscv.addi {rd = x1, rs1 = x0, imm = 240}
# CHECK-NEXT: riscv.srli {rd = x2, rs1 = x1, imm = 2}
# CHECK-NEXT: riscv.slli {rd = x3, rs1 = x1, imm = 1}
# CHECK-NEXT: riscv.or {rd = x4, rs1 = x2, rs2 = x3}
# CHECK-NEXT: riscv.and {rd = x5, rs1 = x3, rs2 = x1}
# CHECK-NEXT: riscv.xor {rd = x6, rs1 = x5, rs2 = x4}
