// RUN: fcc cc -E %S/Inputs/preprocess_input.c | filecheck %s

// CHECK: int answer = 42;
