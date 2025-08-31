    .section .text
shift_test:
    addi x1, x0, 64
    slli x2, x1, 8
    srli x3, x2, 2
    slt  x4, x1, x2
    sltu x5, x1, x3
