// RUN: fcc compile --stage ir -o - %S/../Inputs/void_fn.c | filecheck %s

// A void function with no locals has no stack slots and just returns.

// CHECK: func @nop() {
// CHECK-NEXT: return
// CHECK-NEXT: }
// CHECK-NOT: ptr.alloca

// CHECK: func @implicit() {
// CHECK-NEXT: return
// CHECK-NEXT: }
