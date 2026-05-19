struct Calculator {
    int value;
};

/* Conventional receiver-pointer first param → method on Calculator */
void calc_add(struct Calculator *self, int x) {
    self->value = x;
}

/* Receiver name `this` also recognized */
int calc_get(struct Calculator *this) {
    return this->value;
}

/* First-param name `x` is NOT a recognized receiver — do not bind */
void calc_reset(struct Calculator *x) {
    x->value = 0;
}

/* Plain free function — no receiver shape */
int add(int a, int b) {
    return a + b;
}

int main() {
    struct Calculator c;
    calc_add(&c, 5);
    int v = calc_get(&c);
    calc_reset(&c);
    int s = add(1, 2);
    return v + s;
}
