// `-I` directories are searched in the order given: the header from the first
// `-I` wins over the same-named header in the second.
// RUN: fcc cc -E -I%S/Inputs/order_a -I%S/Inputs/order_b %S/Inputs/order_main.c | filecheck %s

// CHECK: int from_dir_a;
