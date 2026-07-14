int jump(int value) {
    if (value)
        goto done;
    value = 1;
done:
    return value;
}
