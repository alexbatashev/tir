// RUN: fcc compile --stage ast -o - %S/../Inputs/conditional.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       Conditional
// CHECK-NEXT:         Var "condition"
// CHECK-NEXT:         Var "yes"
// CHECK-NEXT:         Var "no"
