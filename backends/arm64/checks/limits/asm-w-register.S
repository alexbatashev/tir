// RUN: tir mc --march=arm64 --filetype=obj-ascii %s | filecheck %s
// XFAIL: *

// UNSUPPORTED (asm path): 32-bit W registers are not accepted by the assembler.
// Any W-register instruction fails with "failed to parse assembly input", so no
// 32-bit-form arithmetic/loads can be written in assembly.

f:
    add w0, w1, w2

// CHECK: .section .text
