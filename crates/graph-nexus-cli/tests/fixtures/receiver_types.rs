struct Dog {
    name: String,
}

struct Cat;

impl Dog {
    fn new() -> Self {
        Dog { name: String::new() }
    }

    fn bark(&self) {
        self.fetch();
    }

    fn fetch(&self) {}
}

impl Cat {
    fn meow(&self) {}
}

trait Animal {
    fn speak(&self);
}

impl Animal for Dog {
    fn speak(&self) {
        self.bark();
    }
}

fn use_param(d: &Dog) {
    d.bark();
}

fn use_let_typed() {
    let d: Dog = Dog::new();
    d.bark();
}

fn use_let_inferred() {
    let d = Dog::new();
    d.bark();
}

fn use_scoped_path() {
    Dog::new();
}
