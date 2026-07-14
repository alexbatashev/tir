#if __STDC_VERSION__ == 7
int selected(void) {
    return 0;
}
#else
#error wrong standard override
#endif
