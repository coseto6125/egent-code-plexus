package com.example;

public class User {
    private long id;
    private String email;
    private String name;

    public User(long id, String email, String name) {
        this.id = id;
        this.email = email;
        this.name = name;
    }

    public long getId() { return id; }
    public String getEmail() { return email; }
    public String getName() { return name; }

    public void setEmail(String email) { this.email = email; }
    public void setName(String name) { this.name = name; }
}
