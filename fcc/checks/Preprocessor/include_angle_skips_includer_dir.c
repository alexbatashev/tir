// An angle include never searches the including file's directory: the sibling
// `angle_hdr.h` is ignored and the `-I` copy is used instead.
// RUN: fcc cc -E -I%S/Inputs/angle_inc %S/Inputs/angle_main.c | filecheck %s

// CHECK: int from_angle_include_dir;
