// RUN: fcc cc -### -c %s -o out.o | filecheck %s

// CHECK: "fcc" "-c" "-o" "out.o" "{{.*}}dry_run_compile.c"
