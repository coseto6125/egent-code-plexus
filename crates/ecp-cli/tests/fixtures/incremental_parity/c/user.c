#include "user.h"
#include <stdlib.h>
#include <string.h>

User *user_new(long id, const char *email, const char *name) {
    User *u = malloc(sizeof(User));
    if (!u) return NULL;
    u->id = id;
    strncpy(u->email, email, sizeof(u->email) - 1);
    strncpy(u->name, name, sizeof(u->name) - 1);
    u->role = ROLE_USER;
    return u;
}

void user_free(User *u) {
    free(u);
}

int user_is_admin(const User *u) {
    return u && u->role == ROLE_ADMIN;
}
