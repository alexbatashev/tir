# RUN: tir mc --march=rv64iv --filetype=obj-ascii %s | filecheck %s --check-prefix=BYTES

# RVV permutations (slides, gathers, compress, scalar moves). Encodings verified
# bit for bit against LLVM's assembler.
.global vpermtest
vpermtest:
    vslideup.vx v1, v2, x13
    vslideup.vx v1, v2, x13, v0.t
    vslideup.vi v1, v2, 5
    vslidedown.vx v1, v2, x13
    vslidedown.vi v1, v2, 5
    vslide1up.vx v1, v2, x13
    vslide1down.vx v1, v2, x13
    vslide1down.vx v1, v2, x13, v0.t
    vrgather.vv v1, v2, v3
    vrgather.vx v1, v2, x13
    vrgather.vi v1, v2, 5
    vrgatherei16.vv v1, v2, v3
    vrgatherei16.vv v1, v2, v3, v0.t
    vcompress.vm v1, v2, v3
    vmv.x.s x5, v2
    vmv.s.x v1, x13

# BYTES: vpermtest:
# BYTES-NEXT: [0xD7, 0xC0, 0x26, 0x3A]
# BYTES-NEXT: [0xD7, 0xC0, 0x26, 0x38]
# BYTES-NEXT: [0xD7, 0xB0, 0x22, 0x3A]
# BYTES-NEXT: [0xD7, 0xC0, 0x26, 0x3E]
# BYTES-NEXT: [0xD7, 0xB0, 0x22, 0x3E]
# BYTES-NEXT: [0xD7, 0xE0, 0x26, 0x3A]
# BYTES-NEXT: [0xD7, 0xE0, 0x26, 0x3E]
# BYTES-NEXT: [0xD7, 0xE0, 0x26, 0x3C]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0x32]
# BYTES-NEXT: [0xD7, 0xC0, 0x26, 0x32]
# BYTES-NEXT: [0xD7, 0xB0, 0x22, 0x32]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0x3A]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0x38]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x5E]
# BYTES-NEXT: [0xD7, 0x22, 0x20, 0x42]
# BYTES-NEXT: [0xD7, 0xE0, 0x06, 0x42]
