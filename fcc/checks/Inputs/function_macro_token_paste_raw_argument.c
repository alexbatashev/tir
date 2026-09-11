#define PREFIX expanded
#define CONCAT(left, right) left ## right
#define FORWARD(left, right) CONCAT(left, right)

int CONCAT(PREFIX, _direct);
int FORWARD(PREFIX, _forwarded);
