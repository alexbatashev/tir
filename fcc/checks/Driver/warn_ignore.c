// Unsupported warning flags are accepted with a stderr warning and do not abort
// the compilation; the preprocessed output still reaches stdout.
// RUN: fcc cc -E -Wall %S/Inputs/preprocess_input.c 2>&1 | filecheck %s

// CHECK: int answer = 42;
// CHECK-DAG: fcc: warning: ignoring unsupported option '-Wall'
