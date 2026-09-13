// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_double_compound_assign.c | filecheck %s

// CHECK: %{{[0-9]+}} = func.func @update(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !f64 {
// CHECK: fp.add
// CHECK: ptr.store
// CHECK: fp.sub
// CHECK: ptr.store
// CHECK: fp.mul
// CHECK: ptr.store
// CHECK: fp.div
// CHECK: ptr.store
