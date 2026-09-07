int printf(const char *format, ...);

int main(void) {
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++)
            printf("%d\n", i + j);

    int grid[4][4];
    for (int i = 0; i < 4; i++)
        for (int j = 0; j < 4; j++)
            grid[i][j] = i * 4 + j;
    int sum = 0;
    for (int i = 0; i < 4; i++)
        for (int j = 0; j < 4; j++)
            sum += grid[i][j];
    printf("%d\n", sum);
    return 0;
}
