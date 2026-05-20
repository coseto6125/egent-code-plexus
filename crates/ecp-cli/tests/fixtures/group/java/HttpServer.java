package demo;
import org.springframework.web.bind.annotation.*;

@RestController
public class HttpServer {
    @PostMapping("/api/users") public String createUser() { return ""; }
    @GetMapping("/api/users/{id}") public String getUser(@PathVariable String id) { return ""; }
}
