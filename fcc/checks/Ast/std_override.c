// RUN: fcc compile -D __STDC_VERSION__=7 --stage ast -o - %S/../Inputs/std_override.c | filecheck %s

// CHECK: Function "selected" -> Int
