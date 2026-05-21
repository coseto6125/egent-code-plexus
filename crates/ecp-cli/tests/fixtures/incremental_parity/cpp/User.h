#pragma once
#include <string>

class User {
public:
    User(long id, std::string email, std::string name);
    long getId() const { return id_; }
    const std::string &getEmail() const { return email_; }
    const std::string &getName() const { return name_; }
    bool isAdmin() const;
    std::string displayName() const;
    void setRole(const std::string &role);

private:
    long id_;
    std::string email_;
    std::string name_;
    std::string role_;
};
