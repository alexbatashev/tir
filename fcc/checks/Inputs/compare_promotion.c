int compare(char narrow) {
    return narrow != 0;
}

int below(unsigned char narrow) {
    return narrow < -1;
}

int negate(int wide) {
    return !(wide >= 0);
}

int truth(_Bool flag) {
    if (flag) {
        return 1;
    }
    return 0;
}
