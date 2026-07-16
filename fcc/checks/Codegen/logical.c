// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_logical.c | filecheck %s

// CHECK: func @logical_and
// CHECK: cir.if
// CHECK: addi
// CHECK: else
// CHECK: constant {value = 0}
// CHECK: func @logical_or
// CHECK: cir.if
// CHECK: constant {value = 1}
// CHECK: else
// CHECK: addi
