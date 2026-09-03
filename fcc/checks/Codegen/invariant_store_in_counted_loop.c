// RUN: fcc compile -O2 --stage asm --march x86_64 -o - %s | filecheck %s

// The loop writes one loop-invariant value to one loop-invariant address, so
// unrolling it leaves copies whose stored values all fold to the one constant.
// They are one write and it has to reach the caller.

void invariant_store(int *p, int a)
{
    for (int i = 0; i < 3; i++) p[a & 3] = i >> 12;
}

// CHECK-LABEL: invariant_store:
// CHECK: mov eax, 0
// CHECK: mov [{{.*}}], eax
// CHECK-NEXT: ret
