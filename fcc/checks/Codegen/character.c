// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_character.c | filecheck %s

// CHECK: func @ordinary
// CHECK: constant {value = 65}
// CHECK: func @escaped
// CHECK: constant {value = 10}
