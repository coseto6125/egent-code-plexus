package com.example;

import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
@RequestMapping("/api")
public class UserController {
    @Autowired
    private UserService userService;

    @Autowired
    private OrderService orderService;

    @GetMapping("/users/{id}")
    public User getUser(Long id) {
        return userService.findById(id);
    }

    @PostMapping("/users")
    public User createUser(User user) {
        return userService.save(user);
    }
}

// 對照組：沒有 @RestController / @Controller，@GetMapping 不該被抓
class NotAController {
    @GetMapping("/x")
    public String notARoute() { return "x"; }
}
