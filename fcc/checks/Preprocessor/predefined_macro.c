// This file was generated with ./utils/scripts/update_checks.py. Do not modify CHECKs manually.

// RUN: fcc compile --stage preprocess -D N=7 -o - %S/../Inputs/predefined_macro.c | filecheck %s

// CHECK: int x = 7;
