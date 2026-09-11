// RUN: fcc compile --stage preprocess -o - %S/../Inputs/function_macro_token_paste_empty_chain.c | filecheck %s

// CHECK: int firstsecond;
