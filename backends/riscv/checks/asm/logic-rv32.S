    .section .text
logic_test:
    addi x1, x0, 0xF0
    srli x2, x1, 2
    slli x3, x1, 1
    or   x4, x2, x3
    and  x5, x3, x1
    xor  x6, x5, x4
