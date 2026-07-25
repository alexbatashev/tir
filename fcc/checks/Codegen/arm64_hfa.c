// RUN: fcc compile --march arm64 --mabi aapcs64 --stage ir -o - %s | filecheck %s
// RUN: fcc compile --march arm64 --mabi aapcs64 --stage asm -o - %s | filecheck %s --check-prefix=ASM

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

double call_consume_pair(struct Pair pair) {
    return consume_pair(pair);
}

struct Quad make_quad(double a, double b, double c, double d) {
    struct Quad result = {{a, b, c, d}};
    return result;
}

// CHECK: func @consume_pair(%{{[0-9]+}}: !tuple<!f64, !f64>) -> !f64 {
// CHECK: func @call_consume_pair(%{{[0-9]+}}: !tuple<!f64, !f64>) -> !f64 {
// CHECK: make_tuple {{.*}} : !tuple<!f64, !f64>
// CHECK: call @consume_pair({{.*}} : !tuple<!f64, !f64>) -> !f64
// CHECK: func @make_quad(
// CHECK-SAME: ) -> !tuple<!f64, !f64, !f64, !f64> {
// CHECK: make_tuple {{.*}} : !tuple<!f64, !f64, !f64, !f64>

// ASM-LABEL: consume_pair:
// ASM: str d0
// ASM: str d1
// ASM-LABEL: call_consume_pair:
// ASM: bl consume_pair
// ASM-LABEL: make_quad:
// ASM: ldr d0
// ASM: ldr d1
// ASM: ldr d2
// ASM: ldr d3
