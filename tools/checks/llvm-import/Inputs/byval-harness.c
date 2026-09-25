#include <stdint.h>

struct S { uint64_t a, b, c, d; };

extern uint64_t sum_byval(struct S value);
extern uint64_t call_sum_byval(struct S *value);

int main(void) {
    struct S value = {11, 22, 33, 44};
    return sum_byval(value) != 55 || call_sum_byval(&value) != 55;
}
