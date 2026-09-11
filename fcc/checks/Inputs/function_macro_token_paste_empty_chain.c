#define CONCAT3(left, middle, right) left ## middle ## right

int CONCAT3(first,,second);
