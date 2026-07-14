// RUN: fcc compile --stage ast -o - %S/../Inputs/increment.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       PostInc
// CHECK-NEXT:         Var "value"
