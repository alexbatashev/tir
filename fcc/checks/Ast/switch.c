// RUN: fcc compile --stage ast -o - %S/../Inputs/switch.c | filecheck %s

// CHECK: Switch
// CHECK-NEXT:       Var "value"
// CHECK-NEXT:       Block
// CHECK-NEXT:         Case
// CHECK-NEXT:           Int 0
// CHECK: Case
// CHECK: Default
