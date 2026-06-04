// This file was generated with ./utils/scripts/update_checks.py. Do not modify CHECKs manually.

// RUN: fcc compile --stage preprocess -o - %S/../Inputs/nested_ifdef_outer_true_inner_false.c | filecheck %s

// CHECK: int b;
// CHECK-NEXT: int c;
