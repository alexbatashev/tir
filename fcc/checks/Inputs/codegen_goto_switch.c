int dispatch(int n, int enter) {
    int total = 0;
    if (enter)
        goto inside;
    switch (n) {
    case 0:
        total = total + 1;
    case 1:
        total = total + 10;
    inside:
        total = total + 100;
        break;
    default:
        total = total + 1000;
    }
    return total;
}
