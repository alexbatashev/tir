# RUN: isasim --march=x86_64 --entry=_start --until-pc=done --trace-registers-end %s | filecheck %s

.global done
done:
  add rax, rax
.global _start
_start:
  mov eax, 0x1fffd
  mov edx, 0x1000
  mov ecx, 0x20000
  idiv ecx

# CHECK: final registers:
# CHECK-DAG: GPR[0] = 0x8000000
# CHECK-DAG: GPR[2] = 0x1fffd
