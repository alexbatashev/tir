// This file was generated with ./utils/scripts/update_checks.py. Do not modify CHECKs manually.

// RUN: fcc compile --stage ast -o - %S/../Inputs/sum.c | filecheck %s

// CHECK: TranslationUnit {
// CHECK-NEXT:     functions: [
// CHECK-NEXT:         Function {
// CHECK-NEXT:             name: "sum",
// CHECK-NEXT:             ret: Int,
// CHECK-NEXT:             params: [
// CHECK-NEXT:                 Param {
// CHECK-NEXT:                     name: "a",
// CHECK-NEXT:                     ty: Int,
// CHECK-NEXT:                 },
// CHECK-NEXT:                 Param {
// CHECK-NEXT:                     name: "b",
// CHECK-NEXT:                     ty: Int,
// CHECK-NEXT:                 },
// CHECK-NEXT:             ],
// CHECK-NEXT:             body: [
// CHECK-NEXT:                 Return(
// CHECK-NEXT:                     Some(
// CHECK-NEXT:                         Binary {
// CHECK-NEXT:                             op: Add,
// CHECK-NEXT:                             lhs: Var(
// CHECK-NEXT:                                 "a",
// CHECK-NEXT:                             ),
// CHECK-NEXT:                             rhs: Var(
// CHECK-NEXT:                                 "b",
// CHECK-NEXT:                             ),
// CHECK-NEXT:                         },
// CHECK-NEXT:                     ),
// CHECK-NEXT:                 ),
// CHECK-NEXT:             ],
// CHECK-NEXT:         },
// CHECK-NEXT:     ],
// CHECK-NEXT: }
