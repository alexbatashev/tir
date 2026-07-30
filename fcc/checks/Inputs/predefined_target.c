#if defined(__arm64__) && !defined(__x86_64__)
int arm64_target;
#else
int wrong_target;
#endif
