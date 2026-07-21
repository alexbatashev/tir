int sum_to(int limit) {
    int sum = 0;
    int value = 0;
again:
    if (value == limit)
        goto done;
    sum += value;
    value = value + 1;
    goto again;
done:
    return sum;
}
