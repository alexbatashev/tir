// RUN: fcc compile --stage ir -o - %S/../Inputs/basic_for.c | filecheck %s

// A counted `for` reaches the mid-end as `scf.for`, not as the tail-controlled
// `scf.while` a flattened loop restructures into: the comparison and the
// increment are the loop op's own bounds and step, so neither is spelled again.

// CHECK-NOT: scf.while
// CHECK: %[[LB:[0-9]+]] = ptr.load
// CHECK: %[[UB:[0-9]+]] = constant {value = 3} : !i32
// CHECK: %[[ST:[0-9]+]] = constant {value = 1} : !i32
// CHECK: scf.for %[[LB]], %[[UB]], %[[ST]] iter_args(
// CHECK-NOT: cmpi
// CHECK-NOT: scf.while
