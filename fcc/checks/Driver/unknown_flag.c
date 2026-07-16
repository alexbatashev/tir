// The error text goes to stderr, which the LIT harness cannot pipe into
// filecheck; the exit status is the observable contract here. The message
// wording is covered by the gcc.rs unit tests.
// RUN: not fcc cc --bogus-flag %s
