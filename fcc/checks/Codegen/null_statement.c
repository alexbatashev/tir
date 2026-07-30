// RUN: fcc compile --stage ir -o - %S/../Inputs/null_statement.c | filecheck %s

// CHECK: func @main() -> !i32
// CHECK: return %{{[0-9]+}}
