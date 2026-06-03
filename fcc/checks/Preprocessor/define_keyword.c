// This file was generated with ./utils/scripts/update_checks.py. Do not modify CHECKs manually.

// RUN: fcc compile --stage preprocess -o - %S/../Inputs/define_keyword.c | filecheck %s

// CHECK: int x;
