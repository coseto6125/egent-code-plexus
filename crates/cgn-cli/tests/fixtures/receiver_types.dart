class Apple {
  void eat() {
    this.peel();
  }
  void peel() {}
}

class Banana extends Apple {
  @override
  void eat() {
    super.eat();
  }
}

void useParam(Apple a) {
  a.peel();
}

void useLocal() {
  Apple a = Apple();
  a.peel();
}

void useNoAnnotation() {
  var a = Apple();
  a.peel();
}
