// RUN: fcc compile --stage ast -o - %S/../Inputs/parenthesized.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       Add
// CHECK-NEXT:         Var "value"
// CHECK-NEXT:         Int 1
