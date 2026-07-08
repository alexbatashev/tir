// RUN: tir mc --march=arm64 --filetype=obj-ascii %s | filecheck %s
// XFAIL: *

// MISCOMPILE (asm path): the plain offset addressing form `ldr xT, [xN, #imm]`
// (no writeback) is assembled as the PRE-INDEX writeback form `ldr xT, [xN,
// #imm]!`, which clobbers the base register xN.
// Reference (aarch64-linux-gnu-as) `ldr x0, [x1, 8]` -> 0xF9400420
//   bytes [0x20, 0x04, 0x40, 0xF9]
// tir emits 0xF8408C20 (pre-index) -> [0x20, 0x8C, 0x40, 0xF8].
// The CHECK asserts the reference encoding, so it XFAILs.

f:
    ldr x0, [x1, 8]

// CHECK: [0x20, 0x04, 0x40, 0xF9]
