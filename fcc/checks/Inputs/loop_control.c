int stop_early(void) {
  int i;
  for (i = 0; i < 4; i = i + 1) {
    if (i == 1) continue;
    if (i == 3) break;
  }
  return i;
}
