// RUN: fcc compile --stage preprocess -o - %S/../Inputs/predefined_version.c | filecheck %s

// CHECK: const char *version = "fcc 0.1.0";
