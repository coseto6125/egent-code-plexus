<?php

class Animal {
    protected string $name;

    public function __construct(string $name) {
        $this->name = $name;
    }
}

class Dog extends Animal {
    public function bark(): void {
        $this->greet();
    }

    public function call_parent(): void {
        parent::__construct("parent_call");
    }

    public function call_self_static(): void {
        self::helper();
        static::helper();
    }

    public static function helper(): void {}

    protected function greet(): void {}
}

class Cat {
    public function meow(): void {
        $this->speak();
    }

    protected function speak(): void {}
}

function standalone(): void {
    $cat = new Cat();
    $cat->meow();
}
