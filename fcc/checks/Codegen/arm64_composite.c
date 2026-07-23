// RUN: fcc compile --march arm64 --mabi aapcs64 --stage ir -o - %s | filecheck %s
// RUN: fcc compile --march arm64 --mabi aapcs64 --stage asm -o - %s | filecheck %s --check-prefix=ASM

struct Mixed {
    double fp;
    long integer;
};

long consume_mixed(struct Mixed value) {
    return value.integer;
}

struct Mixed make_mixed(double fp, long integer) {
    struct Mixed result = {fp, integer};
    return result;
}

// CHECK: func @consume_mixed(%{{[0-9]+}}: !tuple<!i64, !i64>) -> !i64 {
// CHECK: func @make_mixed(
// CHECK-SAME: ) -> !tuple<!i64, !i64> {
// CHECK: make_tuple {{.*}} : !tuple<!i64, !i64>

// ASM-LABEL: consume_mixed:
// ASM: str x0
// ASM: str x1
// ASM-LABEL: make_mixed:
// ASM: ldr x0
// ASM: ldr x1
