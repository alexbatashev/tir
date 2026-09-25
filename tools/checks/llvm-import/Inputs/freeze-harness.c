extern long long frozen_poison_twice(void);
extern long long frozen_value(long long value);

int main(void) {
    return frozen_poison_twice() != 0 ||
           frozen_value(-123) != -123;
}
