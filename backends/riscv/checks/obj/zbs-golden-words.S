# Zbs golden encodings per the Sail RISC-V model.
# RUN: tir mc --march=rv64i_zbs --filetype=obj-ascii %s | filecheck %s

.global f
f:
    bset x10, x11, x12
    bclr x10, x11, x12
    binv x10, x11, x12
    bext x10, x11, x12
    bseti x10, x11, 5
    bclri x10, x11, 5
    binvi x10, x11, 5
    bexti x10, x11, 5

# CHECK: f:
# bset x10, x11, x12 -> 0x28C59533
# CHECK-NEXT: [0x33, 0x95, 0xC5, 0x28]
# bclr x10, x11, x12 -> 0x48C59533
# CHECK-NEXT: [0x33, 0x95, 0xC5, 0x48]
# binv x10, x11, x12 -> 0x68C59533
# CHECK-NEXT: [0x33, 0x95, 0xC5, 0x68]
# bext x10, x11, x12 -> 0x48C5D533
# CHECK-NEXT: [0x33, 0xD5, 0xC5, 0x48]
# bseti x10, x11, 5 -> 0x28559513
# CHECK-NEXT: [0x13, 0x95, 0x55, 0x28]
# bclri x10, x11, 5 -> 0x48559513
# CHECK-NEXT: [0x13, 0x95, 0x55, 0x48]
# binvi x10, x11, 5 -> 0x68559513
# CHECK-NEXT: [0x13, 0x95, 0x55, 0x68]
# bexti x10, x11, 5 -> 0x4855D513
# CHECK-NEXT: [0x13, 0xD5, 0x55, 0x48]
