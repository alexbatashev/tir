#if defined(PROBE_SAME)
double probe(double a, double b, double c) { return a * b + c; }
#elif defined(PROBE_CROSS)
double probe(double a, double b, double c) {
  double product = a * b;
  return product + c;
}
#else
#error missing contraction probe
#endif
