# RUN: isasim --march=arm64 --entry=_start --until-pc=done --trace-instructions %s | filecheck %s

# Parser/decoder coverage for AArch64. The immediate add form reads/writes via
# the `GPRsp` class (slot 31 is `sp`), while the register form uses `GPR`; both
# alias the same physical file. The mnemonic `add` covers both forms, which the
# parser disambiguates by trying each candidate.
.global done
done:
  add x0, x0, x0
.global _start
_start:
  add x1, x0, 5
  add x2, x0, 7
  add x3, x1, x2

# CHECK: add rd=GPRsp[1], rn=GPRsp[0], imm=5
# CHECK-NEXT: add rd=GPRsp[2], rn=GPRsp[0], imm=7
# CHECK-NEXT: add rd=GPR[3], rn=GPR[1], rm=GPR[2]
