// RUN: fcc compile --stage ir -o - %S/../Inputs/basic_do_while.c | filecheck %s

// CHECK: cir.do %{{[0-9]+}} body {
// CHECK: cir.yield
// CHECK: cond {
// CHECK: cir.condition
