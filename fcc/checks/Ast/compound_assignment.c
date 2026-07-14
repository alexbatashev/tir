// RUN: fcc compile --stage ast -o - %S/../Inputs/compound_assignment.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       AddAssign
// CHECK-NEXT:         Var "a"
// CHECK-NEXT:         MulAssign
