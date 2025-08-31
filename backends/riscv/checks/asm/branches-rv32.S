    .section .text
    .global _branch_test
_branch_test:
    addi x1, x0, 3
    addi x2, x0, 3
    bne  x1, x2, mismatch
    addi x3, x0, 1   # equal case
    j    done
mismatch:
    addi x3, x0, -1
done:
    nop
