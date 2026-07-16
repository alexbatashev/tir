// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_cast.c | filecheck %s

// CHECK: func @truncate
// CHECK: trunci
// CHECK: extui
// CHECK: func @widen
// CHECK: extsi
// CHECK: func @widen_unsigned
// CHECK: extui
