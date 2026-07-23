// RUN: fcc compile --march arm64 --mabi aapcs64 --stage ir -o - %s | filecheck %s
// RUN: fcc compile --march arm64 --mabi aapcs64 --stage asm -o - %s | filecheck %s --check-prefix=ASM

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
// CHECK-SAME: ) {
// CHECK: %[[MAKE_DEST:[0-9]+]] = indirect_result : !ptr.p
// CHECK: ptr.memcpy %[[MAKE_DEST]]
// CHECK: return
// CHECK-LABEL: func @forward_large(
// CHECK-SAME: ) {
// CHECK: %[[FORWARD_DEST:[0-9]+]] = indirect_result : !ptr.p
// CHECK: %[[TEMP:[0-9]+]] = ptr.alloca {size = 24, align = 8}
// CHECK: call_indirect_result @make_large(%[[TEMP]]
// CHECK: ptr.memcpy %[[FORWARD_DEST]], %[[TEMP]]
// CHECK: return

// ASM-LABEL: make_large:
// ASM: orr {{x[0-9]+}}, x31, x8
// ASM-LABEL: forward_large:
// ASM: orr {{x[0-9]+}}, x31, x8
// ASM: orr x8, x31, {{x[0-9]+}}
// ASM: bl make_large
// ASM: bl memcpy
