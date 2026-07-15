// RUN: fcc compile --stage ir -o - %S/../Inputs/loop_control.c | filecheck %s

// CHECK: cir.for %[[SCOPE:[0-9]+]] cond {
// CHECK: body {
// CHECK: cir.continue %[[SCOPE]]
// CHECK: cir.break %[[SCOPE]]
