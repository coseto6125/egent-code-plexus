#include "server.h"
#include <stdio.h>
#include <stdlib.h>

Server *server_new(const char *addr, int port) {
    Server *s = malloc(sizeof(Server));
    if (!s) return NULL;
    s->port = port;
    snprintf(s->addr, sizeof(s->addr), "%s", addr);
    s->running = 0;
    return s;
}

int server_start(Server *s) {
    printf("Starting server on %s:%d\n", s->addr, s->port);
    s->running = 1;
    return 0;
}

void server_stop(Server *s) {
    s->running = 0;
}

void server_free(Server *s) {
    free(s);
}
