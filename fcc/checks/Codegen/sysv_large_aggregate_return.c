// RUN: fcc compile --march x86_64 --mabi sysv --stage ir -o - %s | filecheck %s
// RUN: fcc compile --march x86_64 --mabi sysv --stage asm -o - %s | filecheck %s --check-prefix=ASM

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
// CHECK-SAME: ) -> !ptr.p {
// CHECK: %[[MAKE_DEST:[0-9]+]] = indirect_result : !ptr.p
// CHECK: ptr.memcpy %[[MAKE_DEST]]
// CHECK: return %[[MAKE_DEST]]
// CHECK-LABEL: func @forward_large(
// CHECK-SAME: ) -> !ptr.p {
// CHECK: %[[FORWARD_DEST:[0-9]+]] = indirect_result : !ptr.p
// CHECK: %[[TEMP:[0-9]+]] = ptr.alloca {size = 24, align = 8}
// CHECK: %{{[0-9]+}} = call_indirect_result @make_large(%[[TEMP]]
// CHECK-SAME: ) -> !ptr.p
// CHECK: ptr.memcpy %[[FORWARD_DEST]], %[[TEMP]]
// CHECK: return %[[FORWARD_DEST]]

// ASM-LABEL: make_large:
// ASM: mov [[MAKE_DEST:r(bx|1[2-5])]], rdi
// ASM: call memcpy
// ASM: mov rax, [[MAKE_DEST]]
// ASM-LABEL: forward_large:
// ASM: mov [[FORWARD_DEST:r(bx|1[2-5])]], rdi
// ASM: mov rdi, {{r[a-z0-9]+}}
// ASM: call make_large
// ASM: call memcpy
// ASM: mov rax, [[FORWARD_DEST]]
