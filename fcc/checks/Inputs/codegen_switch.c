int classify(int value) {
    int result = 0;
    switch (value) {
    case 0:
        result = 1;
        break;
    case 1:
        result = 2;
    case 2:
        result += 3;
        break;
    default:
        result = 9;
    }
    return result;
}
