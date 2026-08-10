int find(int *items, int count, int wanted) {
  int index;
  for (index = 0; index < count; index = index + 1) {
    if (items[index] == wanted) {
      return index;
    }
  }
  return -1;
}
