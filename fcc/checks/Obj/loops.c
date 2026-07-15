// RUN: fcc compile --stage obj --march riscv32 -o - %S/../Inputs/basic_for.c | tir readobj - | filecheck %s
// RUN: fcc compile --stage asm --march riscv32 -o - %S/../Inputs/basic_for.c | filecheck %s --check-prefix=ASM
// RUN: fcc compile --stage asm --march riscv32 -o - %S/../Inputs/loop_control.c | filecheck %s --check-prefix=CONTROL

// CHECK: File: ELF32 LSB REL
// CHECK: Symbol count: value=0x0

// ASM: count:
// ASM: slti
// ASM: jal

// CONTROL: stop_early:
// CONTROL: bne
// CONTROL: jal
