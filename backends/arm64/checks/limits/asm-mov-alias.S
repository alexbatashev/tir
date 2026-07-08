// RUN: tir mc --march=arm64 --filetype=obj-ascii %s | filecheck %s
// XFAIL: *

// SILENT DROP (asm path): common register aliases parse without error but emit
// NO instruction (the assembler silently skips mnemonics it does not recognize).
// Affected: `mov`, `mvn`, `neg`, `lsl` (register and immediate forms), `ror`,
// `uxtb`. Here `mov x0, x1` disappears and only `ret` is emitted.
// Reference `mov x0, x1` == `orr x0, xzr, x1` -> 0xAA0103E0 [0xE0, 0x03, 0x01, 0xAA].

f:
    mov x0, x1
    ret x30

// CHECK: [0xE0, 0x03, 0x01, 0xAA]
