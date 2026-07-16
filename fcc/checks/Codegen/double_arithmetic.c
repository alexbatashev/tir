// RUN: fcc compile --stage ir -o - %S/../Inputs/codegen_double_arithmetic.c | filecheck %s

// CHECK: func @add(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !f64 {
// CHECK: addf
// CHECK: func @subtract(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !f64 {
// CHECK: subf
// CHECK: func @multiply(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !f64 {
// CHECK: mulf
// CHECK: func @divide(%{{[0-9]+}}: !f64, %{{[0-9]+}}: !f64) -> !f64 {
// CHECK: divf
