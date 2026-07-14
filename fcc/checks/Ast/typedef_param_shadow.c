// RUN: fcc compile --stage ast -o - %S/../Inputs/typedef_param_shadow.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       Add
// CHECK-NEXT:         Var "word"
