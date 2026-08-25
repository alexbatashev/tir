// RUN: fcc compile --stage ir -o - %s | filecheck %s

// A counted `for` is raised to `scf.for`: the counter leaves its slot for a
// carried port, the bounds and step are read once before the loop, and the value
// the loop ends on is stored back so the code after it still reads a slot.
// Nothing branches — the loop is an operation, not a graph.

int count(int n) {
    int i;
    int total = 0;
    for (i = 0; i < n; i = i + 1) {
        total = total + i;
    }
    return total;
}

// CHECK-NOT: scf.while
// CHECK: %[[LB:[0-9]+]] = ptr.load %[[SLOT:[0-9]+]] : !i32
// CHECK: %[[ST:[0-9]+]] = constant {value = 1} : !i32
// CHECK: %[[FINAL:[0-9]+]] = scf.for %[[LB]], %{{[0-9]+}}, %[[ST]] iter_args(%[[IV:[0-9]+]] = %[[LB]]) -> !i32 {
// CHECK-NEXT: ptr.store %[[IV]], %[[SLOT]]
// CHECK: %[[NEXT:[0-9]+]] = addi %[[IV]], %[[ST]] : !i32
// CHECK-NEXT: scf.yield %[[NEXT]]
// CHECK: ptr.store %[[FINAL]], %[[SLOT]]
// CHECK-NOT: scf.while
