#define OUTER
#ifdef OUTER
#ifdef INNER
int a;
#else
int b;
#endif
#endif
int c;
