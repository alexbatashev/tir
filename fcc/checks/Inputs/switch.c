int classify(int value) {
    switch (value) {
    case 0:
        return 1;
    case 1:
    case 2:
        return 2;
    default:
        return 0;
    }
}
