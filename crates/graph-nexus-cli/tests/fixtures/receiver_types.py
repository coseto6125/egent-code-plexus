"""Fixture for receiver-type-binding tests.

Two classes share a method name (`eat`). With type annotations, the parser
must bind each call to the correct class (Apple.eat or Banana.eat) instead
of emitting the bare `eat` short name.
"""


class Apple:
    def eat(self):
        return "apple eaten"

    def peel(self):
        return "apple peeled"


class Banana:
    def eat(self):
        return "banana eaten"


def use_local_annotation():
    x: Apple = Apple()
    x.eat()  # → Apple.eat


def use_param_annotation(b: Banana):
    b.eat()  # → Banana.eat


def use_mixed(a: Apple, unannotated):
    a.peel()           # → Apple.peel
    unannotated.eat()  # → bare `eat` (no type info, fallback)


def use_no_annotation():
    y = Apple()
    y.eat()  # → bare `eat` (annotation absent)


def use_generic_annotation(items: list[Apple]):
    items.append(Apple())  # → bare `append` (generic type, not single identifier)


def use_closure_inheritance(outer: Apple):
    def inner():
        outer.peel()  # → Apple.peel (closure inherits outer scope's type)

    inner()
