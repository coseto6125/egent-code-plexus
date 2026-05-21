#include "User.h"
#include <utility>

User::User(long id, std::string email, std::string name)
    : id_(id), email_(std::move(email)), name_(std::move(name)), role_("user") {}

bool User::isAdmin() const { return role_ == "admin"; }

std::string User::displayName() const {
    return name_ + " <" + email_ + ">";
}

void User::setRole(const std::string &role) { role_ = role; }
