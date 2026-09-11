// RUN: fcc compile --stage ast -o - %s | filecheck %s

#include <math.h>

double call_sin(double value) {
    return sin(value);
}

// CHECK: TranslationUnit
// CHECK: Function "call_sin"
