// RUN: fcc compile -O2 --stage ir -o - %s | filecheck %s

// `a = a` makes the store to the slot write what a load of the same slot
// produced, so one load's reaching value is another load's result. Promotion
// retires both in one sweep, and the second load has to be handed what the
// first was replaced by: handing it the retired value leaves a live operand
// naming a value that no longer exists.

int carry(int a)
{
    int loc = 0;
    for (int i = 0; i < 2; i++) {
        a = a;
        loc = a;
    }
    return loc;
}

// CHECK: func.func @carry
