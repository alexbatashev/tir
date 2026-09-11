// RUN: fcc compile --stage preprocess -o - %S/../Inputs/function_macro_token_paste_empty.c | filecheck %s

// CHECK: extern int value(double);
