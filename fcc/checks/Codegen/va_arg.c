// RUN: fcc compile --stage ir --march x86_64 -o - %s | filecheck %s

#include <stdarg.h>

int next(int fixed, ...) {
    va_list args;
    va_start(args, fixed);
    int value = va_arg(args, int);
    va_end(args);
    return value;
}

double next_double(int fixed, ...) {
    va_list args;
    va_start(args, fixed);
    double value = va_arg(args, double);
    va_end(args);
    return value;
}

// CHECK: func.func @next(
// CHECK: implicit_arguments 14 entry_sp
// CHECK: constant {value = 48} : !i32
// CHECK: ptr.store
// CHECK: ptr.load
// CHECK: func.func @next_double(
// CHECK: implicit_arguments 14 entry_sp
// CHECK: constant {value = 176} : !i32
// CHECK: ptr.store
// CHECK: ptr.load
