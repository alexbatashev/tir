// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_binary32_literal.c | filecheck %s

// CHECK: func.func @decimal() -> !f32
// CHECK: fp.constant {bits = 1069547520} : !f32
