package main

type User struct {
	ID    int64
	Email string
	Name  string
	Role  string
}

type UserStore interface {
	FindByID(id int64) (*User, error)
	FindAll() ([]*User, error)
	Save(u *User) (*User, error)
	Delete(id int64) error
}

func NewUser(email, name string) *User {
	return &User{Email: email, Name: name, Role: "user"}
}

func (u *User) IsAdmin() bool {
	return u.Role == "admin"
}
