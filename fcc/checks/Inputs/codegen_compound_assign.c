int compound_assign(void) {
    int value = 5;
    value += 3;
    value *= 2;
    value -= 4;
    value <<= 1;
    value >>= 2;
    value &= 7;
    value ^= 3;
    value |= 8;
    return value;
}
