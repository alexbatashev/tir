// RUN: tir mc --march=arm64 --filetype=obj -o - %s | tir readobj - | filecheck %s
// RUN: tir mc --march=arm64 --filetype=obj-ascii %s | filecheck %s --check-prefix=BYTES

.global caller
caller:
    bl external_fn
    ret x30

// The call target is unknown at encode time: the imm26 field stays zero and
// an R_AARCH64_CALL26 relocation against the undefined symbol is emitted.
// CHECK: Symbol caller: value=0x0 size=0x8 bind=GLOBAL type=FUNC section=.text
// CHECK: Symbol external_fn: value=0x0 size=0x0 bind=GLOBAL type=NOTYPE section=UND
// CHECK: Reloc .text+0x0: R_AARCH64_CALL26 external_fn + 0

// BYTES: caller:
// BYTES-NEXT: [0x00, 0x00, 0x00, 0x94]
// BYTES-NEXT: [0xC0, 0x03, 0x5F, 0xD6]
