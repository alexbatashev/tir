# RUN: tir mc --march=rv32i %s | filecheck %s

.global foo
foo:
    addi x1, x0, 5
    lw x2, 4(x1)
    sw x2, 8(x1)

# CHECK:      .text
# CHECK-NEXT: .global foo
# CHECK-NEXT: foo:
# CHECK-NEXT: addi x1, x0, 5
# CHECK-NEXT: lw x2, 4(x1)
# CHECK-NEXT: sw x2, 8(x1)
