// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_float_literal.c | filecheck %s

// CHECK: fp.constant {bits = 4609434218613702656}
// CHECK: fp.constant {bits = 4598175219545276416}
// CHECK: fp.constant {bits = 4611686018427387904}
// CHECK: fp.constant {bits = 4636737291354636288}
