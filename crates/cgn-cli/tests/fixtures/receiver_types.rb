class Animal
  def initialize(name)
    @name = name
  end

  def speak
    "..."
  end
end

class Dog < Animal
  def bark
    self.speak
    self.fetch_ball
  end

  def fetch_ball
    "fetching"
  end
end

module Trainer
  def self.train(animal)
    animal.speak
  end
end

class Cat
  def meow
    self.purr
  end

  def purr
    "purring"
  end
end

def standalone_call
  Dog.new("rex").bark
end
