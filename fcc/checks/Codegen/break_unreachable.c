// RUN: fcc compile --stage ir -o - %S/../Inputs/break_unreachable.c | filecheck %s

// CHECK: body {
// CHECK-NEXT: cir.break %{{[0-9]+}}
// CHECK-NEXT: }
