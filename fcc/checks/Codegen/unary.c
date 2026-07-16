// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_unary.c | filecheck %s

// CHECK: func @negate
// CHECK: subi
// CHECK: func @complement
// CHECK: xori
// CHECK: func @logical_not
// CHECK: cmpi
// CHECK: extui
// CHECK: func @positive
