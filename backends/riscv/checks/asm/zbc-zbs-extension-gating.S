# Zbc and Zbs extension gating: mnemonics must not be accepted without the ISA.
# RUN: not tir mc --march=rv32i %s
# RUN: not tir mc --march=rv64i %s

.global f
f:
    clmul x10, x11, x12
    bset x10, x11, x12
