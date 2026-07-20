int increment_values(void) {
    int value = 4;
    int post = value++;
    int pre = ++value;
    int old = value--;
    int now = --value;
    return post + pre + old + now + value;
}
