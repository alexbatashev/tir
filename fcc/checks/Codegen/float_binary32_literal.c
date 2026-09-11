// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_binary32_literal.c | filecheck %s

// CHECK: func.func @decimal() -> !f32
// CHECK: constantf {value = 1.5} : !f32
