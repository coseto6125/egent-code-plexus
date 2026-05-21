#ifndef USER_H
#define USER_H

typedef enum { ROLE_USER = 0, ROLE_ADMIN = 1 } Role;

typedef struct {
    long id;
    char email[256];
    char name[128];
    Role role;
} User;

User *user_new(long id, const char *email, const char *name);
void user_free(User *u);
int user_is_admin(const User *u);

#endif
