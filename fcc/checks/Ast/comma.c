// RUN: fcc compile --stage ast -o - %S/../Inputs/comma.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       Comma
// CHECK-NEXT:         AssignExpr
// CHECK-NEXT:           Var "a"
// CHECK-NEXT:           Var "b"
// CHECK-NEXT:         Var "c"
