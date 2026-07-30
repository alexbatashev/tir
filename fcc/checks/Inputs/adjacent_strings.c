#define PREFIX "macro "

char *same_line(void) {
    return "same " "line";
}

char *cross_line(void) {
    return "cross "
           "line";
}

char *macro_expansion(void) {
    return PREFIX "expansion";
}
