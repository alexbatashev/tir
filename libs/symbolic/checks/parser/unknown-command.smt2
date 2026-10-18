; RUN: not tir-smt %s 2>&1 | filecheck %s

(get-proof)

; CHECK: error: {{.*}}unknown command `get-proof`
