protocol UserRepository {
    func findById(_ id: Int64) -> User?
    func findAll() -> [User]
    func save(_ user: User) -> User
    func delete(_ id: Int64) -> Bool
}

class UserService {
    private let repository: UserRepository

    init(repository: UserRepository) {
        self.repository = repository
    }

    func getUser(id: Int64) -> User? {
        repository.findById(id)
    }

    func listUsers() -> [User] {
        repository.findAll()
    }

    func createUser(email: String, name: String) -> User {
        let user = User(id: Int64(Date().timeIntervalSince1970), email: email, name: name)
        return repository.save(user)
    }

    func deleteUser(id: Int64) -> Bool {
        repository.delete(id)
    }
}
