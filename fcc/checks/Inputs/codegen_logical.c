int logical_and(int lhs) {
    int rhs = 0;
    int result = lhs && ++rhs;
    return result * 10 + rhs;
}

int logical_or(int lhs) {
    int rhs = 0;
    int result = lhs || ++rhs;
    return result * 10 + rhs;
}
