class Apple {
    func eat() {
        self.peel()
    }
    func peel() {}
}

class Banana: Fruit {
    override func eat() {
        super.eat()
    }
}

extension Apple {
    func slice() {
        self.peel()
    }
}

func useParam(a: Apple) {
    a.peel()
}

func useLocal() {
    let a: Apple = Apple()
    a.peel()
}

func useNoAnnotation() {
    let a = Apple()
    a.peel()
}
