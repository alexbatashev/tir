// RUN: fcc compile --std c99 --stage preprocess -o - %s | filecheck %s

#include <stddef.h>
#include <stdarg.h>

// CHECK: int system_headers_found;
int system_headers_found;
