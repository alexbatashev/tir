// A header included via a quoted path resolves nested quoted includes against
// its own directory, exercising the resolved-path tracking of include frames.
// RUN: fcc cc -E %S/Inputs/nested_main.c | filecheck %s

// CHECK: int from_inner;
// CHECK: int from_outer;
