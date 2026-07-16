// A quoted include resolves against the directory of the including file, so a
// sibling header is found with no `-I` on the command line.
// RUN: fcc cc -E %S/Inputs/quoted_main.c | filecheck %s

// CHECK: int from_quoted_sibling;
