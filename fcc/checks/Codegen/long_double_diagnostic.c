// RUN: not fcc compile --stage ir -o - %s 2>&1 | filecheck %s

long double add(long double value) {
    return value + value;
}

// CHECK: codegen not yet implemented for long double expressions
