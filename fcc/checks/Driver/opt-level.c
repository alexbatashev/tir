// The level picks a mid-end, and the mid-end is observable: `x + 0` survives
// the level that runs no round and folds under every level that runs one.
// RUN: fcc cc -O0 -S -o - %s | filecheck %s --check-prefix=O0
// RUN: fcc cc -S -o - %s | filecheck %s --check-prefix=O0
// RUN: fcc cc -O1 -S -o - %s | filecheck %s --check-prefix=OPT
// RUN: fcc cc -O2 -S -o - %s | filecheck %s --check-prefix=OPT
// RUN: fcc cc -Os -Os -S -o - %s 2>&1 | filecheck %s --check-prefix=ALIAS

int add_zero(int x) { return x + 0; }

// A level with no round leaves the frontend's slots in memory.
// O0-LABEL: add_zero:
// O0: mov [rax], edi

// OPT-LABEL: add_zero:
// OPT-NOT: mov [rax]
// OPT: ret

// -Os aliases -O2, and says so once.
// ALIAS: fcc: warning: treating '-Os' as '-O2'
// ALIAS-NOT: fcc: warning: treating '-Os' as '-O2'
