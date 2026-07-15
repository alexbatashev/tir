struct Pair {
    char tag;
    int value;
};

int read(struct Pair *pair) {
    return pair->value;
}

int copy(void) {
    struct Pair source;
    struct Pair destination;
    source.value = 37;
    destination = source;
    return destination.value;
}
