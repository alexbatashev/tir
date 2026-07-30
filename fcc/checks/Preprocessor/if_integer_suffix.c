// RUN: fcc compile --std c99 --stage preprocess -o - %S/../Inputs/if_integer_suffix.c | filecheck %s

// CHECK: int c99_branch;
// CHECK: int unsigned_branch;
// CHECK: int unsigned_long_branch;
// CHECK: int long_long_branch;
// CHECK: int unsigned_long_long_branch;
