int printf(const char *format, ...);

int print_float(float value) {
    return printf("%.1f", value);
}
