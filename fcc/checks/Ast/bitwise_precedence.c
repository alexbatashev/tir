// RUN: fcc compile --stage ast -o - %S/../Inputs/bitwise_precedence.c | filecheck %s

// CHECK: Return
// CHECK-NEXT:       LogAnd
// CHECK-NEXT:         BitOr
// CHECK-NEXT:           BitXor
// CHECK-NEXT:             BitAnd
// CHECK-NEXT:               Shl
// CHECK-NEXT:                 Add
