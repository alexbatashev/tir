// RUN: fcc compile --stage ir -o - %S/../Inputs/basic_if.c | filecheck %s

// CHECK: cir.if %{{[0-9]+}} {
// CHECK: ptr.store
// CHECK: cir.yield
// CHECK: else {
// CHECK: ptr.store
// CHECK: cir.yield
