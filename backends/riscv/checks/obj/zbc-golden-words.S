# Zbc golden encodings per the Sail RISC-V model.
# RUN: tir mc --march=rv64i_zbc --filetype=obj-ascii %s | filecheck %s

.global f
f:
    clmul x10, x11, x12
    clmulh x10, x11, x12
    clmulr x10, x11, x12

# CHECK: f:
# clmul x10, x11, x12 -> 0x0AC59533
# CHECK-NEXT: [0x33, 0x95, 0xC5, 0x0A]
# clmulh x10, x11, x12 -> 0x0AC5B533
# CHECK-NEXT: [0x33, 0xB5, 0xC5, 0x0A]
# clmulr x10, x11, x12 -> 0x0AC5A533
# CHECK-NEXT: [0x33, 0xA5, 0xC5, 0x0A]
