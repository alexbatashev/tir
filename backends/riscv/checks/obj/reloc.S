# RUN: tir mc --march=rv64i --filetype=obj -o - %s | tir readobj - | filecheck %s
# RUN: tir mc --march=rv64i --filetype=obj-ascii %s | filecheck %s --check-prefix=BYTES

.global caller
caller:
    jal x1, external_fn
    jalr x0, 0(x1)

# The call target is unknown at encode time: the J-type immediate stays zero
# and an R_RISCV_JAL relocation against the undefined symbol is emitted.
# CHECK: Symbol caller: value=0x0 size=0x8 bind=GLOBAL type=FUNC section=.text
# CHECK: Symbol external_fn: value=0x0 size=0x0 bind=GLOBAL type=NOTYPE section=UND
# CHECK: Reloc .text+0x0: R_RISCV_JAL external_fn + 0

# BYTES: caller:
# BYTES-NEXT: [0xEF, 0x00, 0x00, 0x00]
# BYTES-NEXT: [0x67, 0x80, 0x00, 0x00]
