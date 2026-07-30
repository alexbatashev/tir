// RUN: fcc compile --stage preprocess -o - %S/../Inputs/function_macro_token_paste.c | filecheck %s

// CHECK: int value7 = 7;
