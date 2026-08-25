// RUN: fcc compile --stage ir -o - %s | filecheck %s

// A `+=` step counts like any other: the constant it adds becomes the loop's
// step, and a bound read from a parameter's slot is read once before the loop.

int advance(int limit) {
    int value;
    for (value = 0; value < limit; value += 2) {
    }
    return value;
}

// CHECK: %[[LB:[0-9]+]] = ptr.load %{{[0-9]+}} : !i32
// CHECK-NEXT: %[[UB:[0-9]+]] = ptr.load %{{[0-9]+}} : !i32
// CHECK-NEXT: %[[ST:[0-9]+]] = constant {value = 2} : !i32
// CHECK-NEXT: scf.for %[[LB]], %[[UB]], %[[ST]] iter_args(
