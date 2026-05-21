package com.example;

import java.util.List;

public class ApiController {
    private final UserService userService;

    public ApiController(UserService userService) {
        this.userService = userService;
    }

    public List<User> listUsers() {
        return userService.findAll();
    }

    public User getUser(long id) {
        return userService.findById(id).orElseThrow();
    }

    public User createUser(String email, String name) {
        return userService.save(new User(0, email, name));
    }

    public void removeUser(long id) {
        userService.delete(id);
    }
}
