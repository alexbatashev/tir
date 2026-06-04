    .section .text
    .global _exit
_exit:
    addi x10, x0, 0   # return code
    addi x17, x0, 93  # ECALL: exit
    ecall
