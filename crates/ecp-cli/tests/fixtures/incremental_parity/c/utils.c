#include "utils.h"
#include <ctype.h>
#include <string.h>

void slugify(const char *input, char *output, size_t out_len) {
    size_t i = 0, j = 0;
    while (input[i] && j < out_len - 1) {
        char c = tolower((unsigned char)input[i]);
        output[j++] = isalnum((unsigned char)c) ? c : '-';
        i++;
    }
    output[j] = '\0';
}

int is_valid_email(const char *email) {
    const char *at = strchr(email, '@');
    if (!at || at == email) return 0;
    const char *dot = strchr(at + 1, '.');
    return dot && *(dot + 1) != '\0';
}

void truncate_str(const char *input, char *output, size_t max_len) {
    if (strlen(input) <= max_len) {
        strcpy(output, input);
    } else {
        strncpy(output, input, max_len);
        strcpy(output + max_len, "...");
    }
}
