// RUN: fcc compile --march arm64 --stage preprocess -o - %S/../Inputs/predefined_target.c | filecheck %s

// CHECK: int arm64_target;
