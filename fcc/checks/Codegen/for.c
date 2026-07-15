// RUN: fcc compile --stage ir -o - %S/../Inputs/basic_for.c | filecheck %s

// CHECK: cir.for %{{[0-9]+}} cond {
// CHECK: cir.condition
// CHECK: body {
// CHECK: cir.yield
// CHECK: step {
// CHECK: ptr.store
// CHECK: cir.yield
