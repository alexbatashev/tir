// A missing include is a hard error. The message goes to stderr, which the LIT
// harness cannot pipe into filecheck; the failing exit status is the observable
// contract here. The message wording is covered by a preprocessor unit test.
// RUN: not fcc cc -E %S/Inputs/missing_main.c
