extern long long frozen_twice(long long value);
extern long long frozen_poison_twice(void);

int main(void) {
    return frozen_twice(123) != 0 || frozen_poison_twice() != 0;
}
