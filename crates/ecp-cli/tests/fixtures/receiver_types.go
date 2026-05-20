package zoo

type Dog struct {
	Name string
}

type Cat struct {
	Name string
}

func (d *Dog) Bark() {
	d.Fetch()
}

func (d *Dog) Fetch() {}

func (c *Cat) Meow() {}

func UseParam(d *Dog) {
	d.Bark()
}

func UseVar() {
	var d Dog
	d.Bark()
}

func UseShortVar() {
	d := Dog{Name: "Spot"}
	d.Bark()
}

func UseNoType() {
	d := makeDog()
	d.Bark()
}

func makeDog() *Dog { return &Dog{} }
