// RUN: fcc compile --stage tokens -o - %S/../Inputs/increment.c | filecheck %s

// CHECK: KwReturn,
// CHECK: Identifier(
// CHECK-NEXT:         "value",
// CHECK-NEXT:     ),
// CHECK: PlusPlus,
