// RUN: fcc compile --stage ir -o - %S/../Inputs/loop_control.c | filecheck %s

// A `continue` in a `for` raises a flag and the rest of the body is guarded by
// it, because the step trails the body once the loop is structured and C runs
// the step on the way to the next iteration. A `break` skips the step, which is
// what leaving the loop scope already means.

// CHECK: cir.for %[[SCOPE:[0-9]+]] cond {
// CHECK: body {
// CHECK-NEXT: %[[ZERO:[0-9]+]] = constant {value = 0}
// CHECK-NEXT: ptr.store %[[ZERO]], %[[FLAG:[0-9]+]]
// CHECK: cir.if
// CHECK: %[[ONE:[0-9]+]] = constant {value = 1}
// CHECK-NEXT: ptr.store %[[ONE]], %[[FLAG]]
// CHECK: ptr.load %[[FLAG]]
// CHECK: cir.if
// CHECK: cir.break %[[SCOPE]]
// CHECK: step {
// CHECK-NOT: cir.continue
