class Apple {
public:
    void eat() {
        this->peel();
    }
    void peel() {}
};

class Banana : public Apple {
public:
    void eat() {
        Apple::eat();
    }
};

void useParam(Apple a) {
    a.eat();
}

void useLocal() {
    Apple a;
    a.eat();
    Apple* p = &a;
    p->eat();
}

void useNoAnnotation() {
    auto a = Apple();
    a.eat();
}
