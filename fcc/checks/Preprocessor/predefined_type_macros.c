// RUN: fcc compile -O0 --stage ir --march x86_64 -o - %s | filecheck %s

// The type macros name a type, which is more than one token, so a predefine
// carries a replacement the preprocessor lexes rather than a single token.
// Without them a declaration reading `__SIZE_TYPE__` is an identifier list,
// and the call passes an `int` where the callee takes a `long`.

extern void takes_size(__SIZE_TYPE__);
extern void takes_ptrdiff(__PTRDIFF_TYPE__);

void call(void)
{
    takes_size(100);
    takes_ptrdiff(100);
}

// CHECK-LABEL: func.func @call
// CHECK: func.call {{.*}}!i64
// CHECK: func.call {{.*}}!i64
