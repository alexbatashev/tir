// RUN: fcc compile --stage preprocess -o - %S/../Inputs/function_macro_token_paste_raw_argument.c | filecheck %s

// CHECK: int PREFIX_direct;
// CHECK: int expanded_forwarded;
