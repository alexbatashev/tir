unsigned int bits(unsigned int a, unsigned int b) {
    return ((a & b) | (a ^ b)) << 2 >> 1;
}

int signed_shift(int value) {
    return value >> 3;
}
