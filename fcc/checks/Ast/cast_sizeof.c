// RUN: fcc compile --stage ast -o - %S/../Inputs/cast_sizeof.c | filecheck %s

// CHECK: Cast Long
// CHECK-NEXT:           Var "value"
// CHECK: SizeofType Short
// CHECK: SizeofExpr
// CHECK-NEXT:         Var "value"
