import Foundation

func slugify(_ text: String) -> String {
    let lower = text.lowercased()
    let slug = lower.components(separatedBy: CharacterSet.alphanumerics.inverted)
        .filter { !$0.isEmpty }
        .joined(separator: "-")
    return slug
}

func truncate(_ text: String, maxLen: Int) -> String {
    guard text.count > maxLen else { return text }
    let index = text.index(text.startIndex, offsetBy: maxLen)
    return String(text[..<index]) + "..."
}

func isValidEmail(_ email: String) -> Bool {
    email.contains("@") && email.contains(".")
}
