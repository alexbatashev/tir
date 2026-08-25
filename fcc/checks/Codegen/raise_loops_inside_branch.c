// RUN: fcc compile --stage ir -o - %s | filecheck %s

// A raised loop is an ordinary non-terminator operation sitting in a block, so
// the blocks around it still restructure: the `if` here reaches `restructure` as
// a CFG diamond and comes back as `scf.if` carrying the `scf.for` along. Nothing
// branches in the result.

int pick(int flag, int n) {
    int i;
    int total = 0;
    if (flag) {
        for (i = 0; i < n; i = i + 1) {
            total = total + i;
        }
    } else {
        total = 1;
    }
    return total;
}

// CHECK: func.func @pick
// CHECK: scf.if
// CHECK: scf.for %{{[0-9]+}}, %{{[0-9]+}}, %{{[0-9]+}} iter_args(
// CHECK: scf.yield
// CHECK: else
// CHECK-NOT: cfg.
// CHECK-NOT: scf.while
