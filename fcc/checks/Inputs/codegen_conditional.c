int conditional(int condition) {
    int lhs = 0;
    int rhs = 0;
    int result = condition ? ++lhs : ++rhs;
    return result * 100 + lhs * 10 + rhs;
}
