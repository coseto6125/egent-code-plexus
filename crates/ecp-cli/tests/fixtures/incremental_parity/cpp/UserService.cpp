#include "UserService.h"
#include <algorithm>
#include <stdexcept>

UserService::UserService(UserRepository &repo) : repo_(repo) {}

std::optional<User> UserService::findById(long id) {
    return repo_.findById(id);
}

std::vector<User> UserService::findAll() {
    return repo_.findAll();
}

User UserService::create(const std::string &email, const std::string &name) {
    User u(0, email, name);
    return repo_.save(u);
}

bool UserService::remove(long id) {
    return repo_.deleteById(id);
}
