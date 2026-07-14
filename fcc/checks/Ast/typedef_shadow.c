// RUN: fcc compile --stage ast -o - %S/../Inputs/typedef_shadow.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       Add
// CHECK-NEXT:         Var "word"
// CHECK-NEXT:         Int 1
