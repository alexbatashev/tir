#if 199901L > 201710L
int wrong_stdc_branch;
#else
int c99_branch;
#endif

#if 1U > 2U
int wrong_unsigned_branch;
#else
int unsigned_branch;
#endif

#if 0x10UL > 0x20LU
int wrong_unsigned_long_branch;
#else
int unsigned_long_branch;
#endif

#if 1LL > 2LL
int wrong_long_long_branch;
#else
int long_long_branch;
#endif

#if 0x10ULL > 0x20LLU
int wrong_unsigned_long_long_branch;
#else
int unsigned_long_long_branch;
#endif
