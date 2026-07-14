// RUN: fcc compile --stage ast -o - %S/../Inputs/assignment_value.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       AssignExpr
// CHECK-NEXT:         Var "a"
// CHECK-NEXT:         AssignExpr
// CHECK-NEXT:           Var "b"
// CHECK-NEXT:           Var "c"
