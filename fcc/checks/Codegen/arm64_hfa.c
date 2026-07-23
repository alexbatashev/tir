// RUN: fcc compile --march arm64 --mabi aapcs64 --stage ir -o - %s | filecheck %s

struct Pair {
    double left;
    double right;
};

struct Quad {
    double values[4];
};

double consume_pair(struct Pair pair) {
    return pair.left + pair.right;
}

struct Quad make_quad(double a, double b, double c, double d) {
    struct Quad result = {{a, b, c, d}};
    return result;
}

// CHECK: func @consume_pair(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !f64 {
// CHECK: func @make_quad(
// CHECK-SAME: ) -> !tuple<!f64, !f64, !f64, !f64> {
// CHECK: make_tuple {{.*}} : !tuple<!f64, !f64, !f64, !f64>
