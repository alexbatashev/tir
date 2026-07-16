// Unsupported optimization/warning flags are accepted with a stderr warning and
// do not abort the compilation; the preprocessed output still reaches stdout.
// RUN: fcc cc -E -O2 -Wall %S/Inputs/preprocess_input.c | filecheck %s

// CHECK: int answer = 42;
