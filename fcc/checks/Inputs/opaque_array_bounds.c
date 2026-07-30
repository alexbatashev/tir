struct Item {
  int value;
};

extern struct Item item;

int aligned[__alignof__(item) >= __alignof__(int) ? 1 : -1];
int compound[sizeof (struct Item){ 1 } == sizeof(struct Item) ? 1 : -1];

struct Buffer {
  char data[sizeof(struct Item) - __builtin_offsetof(struct Item, value)];
};
