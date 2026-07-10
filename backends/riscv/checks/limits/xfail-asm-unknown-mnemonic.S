# RUN: not tir mc --march=rv64i --filetype=obj-ascii %s

# Unknown assembly mnemonics are hard parse errors.

.global f
f:
    addi x1, x2, 1
    foobar x1, x2, x3
    addi x4, x5, 2
