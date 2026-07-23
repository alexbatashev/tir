// RUN: fcc compile --march riscv64 --mabi lp64d --stage ir -o - %s | filecheck %s
// RUN: fcc compile --march riscv64 --mabi lp64d --stage asm -o - %s | filecheck %s --check-prefix=ASM

struct Scalar {
    double value;
};

struct Pair {
    double left;
    double right;
};

struct Scalar make_scalar(double value) {
    struct Scalar result = {value};
    return result;
}

struct Pair make_pair(double left, double right) {
    struct Pair result = {left, right};
    return result;
}

struct Pair external_pair(double, double);

double call_external_pair(void) {
    struct Pair result = external_pair(1.0, 2.0);
    return result.left + result.right;
}

// CHECK: func @make_scalar(%{{[0-9]+}}: !f64) -> !f64 {
// CHECK: return %{{[0-9]+}}
// CHECK: func @make_pair(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !tuple<!f64, !f64> {
// CHECK: %[[PAIR:[0-9]+]] = make_tuple %{{[0-9]+}}, %{{[0-9]+}} : !tuple<!f64, !f64>
// CHECK: return %[[PAIR]]
// CHECK: declare @external_pair(!f64, !f64) -> !tuple<!f64, !f64>
// CHECK: func @call_external_pair() -> !f64 {
// CHECK: %[[CALL:[0-9]+]] = call @external_pair({{.*}}) -> !tuple<!f64, !f64>
// CHECK: tuple_get %[[CALL]] {index = 0} : !f64
// CHECK: tuple_get %[[CALL]] {index = 1} : !f64

// ASM-LABEL: make_pair:
// ASM: fld f10, 0({{.*}})
// ASM: fld f11, 8({{.*}})
// ASM-LABEL: call_external_pair:
// ASM: jal x1, external_pair
// ASM: fsd f{{[0-9]+}}, 0({{.*}})
// ASM: fsd f{{[0-9]+}}, 8({{.*}})
