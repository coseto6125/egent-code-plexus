#include <algorithm>
#include <cctype>
#include <string>

std::string slugify(const std::string &text) {
    std::string result;
    result.reserve(text.size());
    for (char c : text) {
        if (std::isalnum(static_cast<unsigned char>(c))) {
            result += static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
        } else if (!result.empty() && result.back() != '-') {
            result += '-';
        }
    }
    while (!result.empty() && result.back() == '-') result.pop_back();
    return result;
}

bool isValidEmail(const std::string &email) {
    auto at = email.find('@');
    if (at == std::string::npos || at == 0) return false;
    return email.find('.', at + 1) != std::string::npos;
}

std::string truncate(const std::string &text, size_t maxLen) {
    if (text.size() <= maxLen) return text;
    return text.substr(0, maxLen) + "...";
}
