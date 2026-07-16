// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_bitwise_shift.c | filecheck %s

// CHECK: func @bits
// CHECK: andi
// CHECK: xori
// CHECK: ori
// CHECK: shli
// CHECK: shrui
// CHECK: func @signed_shift
// CHECK: shrsi
