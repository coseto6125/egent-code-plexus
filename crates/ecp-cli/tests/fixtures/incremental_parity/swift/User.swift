import Foundation

struct User {
    let id: Int64
    var email: String
    var name: String
    var role: String

    init(id: Int64, email: String, name: String, role: String = "user") {
        self.id = id
        self.email = email
        self.name = name
        self.role = role
    }

    var isAdmin: Bool { role == "admin" }
    var displayName: String { "\(name) <\(email)>" }
}
