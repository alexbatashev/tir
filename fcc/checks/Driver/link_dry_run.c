// RUN: fcc cc -### %s %S/Inputs/other.c -o a.out -L/x -lm | filecheck %s

// CHECK: "fcc" "-c" "-o" "{{.*}}link_dry_run{{.*}}.o" "{{.*}}link_dry_run.c"
// CHECK: "fcc" "-c" "-o" "{{.*}}other{{.*}}.o" "{{.*}}other.c"
// CHECK: "cc" "-o" "a.out" "{{.*}}link_dry_run{{.*}}.o" "{{.*}}other{{.*}}.o" "-L/x" "-lm"
