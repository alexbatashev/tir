# RUNx: tir mc --march=rv32i --stage=regalloc %s | filecheck %s

    .section .rodata
msg: .string "Hello, RISC-V!"

    .section .bss
    .align 4
buffer: .space 16

    .section .text
    .global _memtest
_memtest:
    la   x1, msg
    la   x2, buffer
    lw   x3, 0(x1)
    sw   x3, 0(x2)
    nop
