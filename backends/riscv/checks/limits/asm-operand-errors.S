# RUN: not tir mc --march=rv64i --filetype=obj-ascii %s

# The assembler rejects an I-type immediate outside the signed 12-bit field
# (4096 does not fit addi) instead of truncating. Register and arity errors are
# rejected the same way; the boundary values 2047/-2048 assemble (see
# arithmetic-rv64.S).

.global f
f:
    addi x1, x2, 4096
