// RUN: fcc compile --march riscv64 --mabi lp64d --stage ir -o - %s | filecheck %s
// RUN: fcc compile --march riscv64 --mabi lp64d --stage asm -o - %s | filecheck %s --check-prefix=ASM

struct Large {
    long values[3];
};

struct Large make_large(long a, long b, long c) {
    struct Large result = {{a, b, c}};
    return result;
}

struct Large forward_large(long a, long b, long c) {
    return make_large(a, b, c);
}

// CHECK-LABEL: func @make_large(
// CHECK-SAME: %{{[0-9]+}}: !i64, %{{[0-9]+}}: !i64, %{{[0-9]+}}: !i64, %{{[0-9]+}}: !i64) {
// CHECK: %[[MAKE_DEST:[0-9]+]] = indirect_result : !ptr.p
// CHECK: ptr.memcpy %[[MAKE_DEST]]
// CHECK: return
// CHECK-LABEL: func @forward_large(
// CHECK-SAME: %{{[0-9]+}}: !i64, %{{[0-9]+}}: !i64, %{{[0-9]+}}: !i64, %{{[0-9]+}}: !i64) {
// CHECK: %[[FORWARD_DEST:[0-9]+]] = indirect_result : !ptr.p
// CHECK: %[[HIDDEN:[0-9]+]] = constant {value = 0} : !i64
// CHECK: %[[TEMP:[0-9]+]] = ptr.alloca {size = 24, align = 8}
// CHECK: call_indirect_result @make_large(%[[TEMP]], %[[HIDDEN]]
// CHECK: ptr.memcpy %[[FORWARD_DEST]], %[[TEMP]]
// CHECK: return

// ASM-LABEL: make_large:
// ASM: c.mv {{x[0-9]+}}, x11
// ASM-LABEL: forward_large:
// ASM: c.mv x10, {{x[0-9]+}}
// ASM: jal x1, make_large
// ASM: jal x1, memcpy
