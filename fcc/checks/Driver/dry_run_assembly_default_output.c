// RUN: fcc cc -### -S %s | filecheck %s

// CHECK: "fcc" "-S" "-o" "dry_run_assembly_default_output.s" "{{.*}}dry_run_assembly_default_output.c"
