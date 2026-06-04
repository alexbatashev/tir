    .section .data
bigval: .dword 0x100000000

    .section .text
    .global _start64
_start64:
    la   x5, bigval
    ld   x6, 0(x5)
    addi x7, x6, 42
    sd   x7, 0(x5)
