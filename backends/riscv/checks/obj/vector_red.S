# RUN: tir mc --march=rv64iv --filetype=obj-ascii %s | filecheck %s --check-prefix=BYTES

# RVV integer reductions (single-width and widening). Encodings verified bit
# for bit against LLVM's assembler.
.global vredtest
vredtest:
    vredsum.vs v1, v2, v3
    vredsum.vs v1, v2, v3, v0.t
    vredand.vs v1, v2, v3
    vredor.vs v1, v2, v3
    vredxor.vs v1, v2, v3
    vredminu.vs v1, v2, v3
    vredmin.vs v1, v2, v3, v0.t
    vredmaxu.vs v1, v2, v3
    vredmax.vs v1, v2, v3
    vwredsumu.vs v1, v2, v3
    vwredsumu.vs v1, v2, v3, v0.t
    vwredsum.vs v1, v2, v3
    vwredsum.vs v1, v2, v3, v0.t

# BYTES: vredtest:
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x02]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x00]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x06]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x0A]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x0E]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x12]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x14]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x1A]
# BYTES-NEXT: [0xD7, 0xA0, 0x21, 0x1E]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0xC2]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0xC0]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0xC6]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0xC4]
