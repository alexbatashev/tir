// RUN: tir mc --march=arm64 --filetype=obj-ascii %s | filecheck %s
// XFAIL: *

// MISCOMPILE (asm path): a shifted register operand is parsed but the shift is
// silently dropped. `add x0, x1, x2, lsl 3` assembles as plain `add x0, x1, x2`.
// Reference (aarch64-linux-gnu-as) -> 0x8B020C20 [0x20, 0x0C, 0x02, 0x8B].
// tir emits 0x8B020020 [0x20, 0x00, 0x02, 0x8B] (shift amount 0).

f:
    add x0, x1, x2, lsl 3

// CHECK: [0x20, 0x0C, 0x02, 0x8B]
