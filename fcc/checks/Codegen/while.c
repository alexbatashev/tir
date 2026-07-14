// RUN: fcc compile --stage ir -o - %S/../Inputs/basic_while.c | filecheck %s

// CHECK: cir.while %{{[0-9]+}} cond {
// CHECK: cir.condition
// CHECK: body {
// CHECK: cir.yield
