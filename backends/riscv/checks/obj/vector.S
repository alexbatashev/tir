# RUN: tir mc --march=rv64iv --filetype=obj-ascii %s | filecheck %s --check-prefix=BYTES

# A small slice of the RVV instructions. The encodings match the RISC-V Vector
# spec bit for bit, e.g. `vadd.vv v1, v2, v3` is 0x022180d7 and
# `vsetivli x0, 4, 208` (208 = e32/m1/ta/ma) is 0xcd027057.
.global vtest
vtest:
    vsetivli x0, 4, 208
    vsetvli x5, x1, 208
    vadd.vv v1, v2, v3
    vsub.vv v4, v5, v6
    vmul.vv v7, v8, v9

# BYTES: vtest:
# BYTES-NEXT: [0x57, 0x70, 0x02, 0xCD]
# BYTES-NEXT: [0xD7, 0xF2, 0x00, 0x0D]
# BYTES-NEXT: [0xD7, 0x80, 0x21, 0x02]
# BYTES-NEXT: [0x57, 0x02, 0x53, 0x0A]
# BYTES-NEXT: [0xD7, 0x83, 0x84, 0x96]
