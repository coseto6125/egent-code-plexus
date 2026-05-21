import 'user.dart';

abstract class UserRepository {
  Future<User?> findById(int id);
  Future<List<User>> findAll();
  Future<User> save(User user);
  Future<bool> delete(int id);
}

class UserService {
  final UserRepository _repository;

  UserService(this._repository);

  Future<User?> getUser(int id) => _repository.findById(id);

  Future<List<User>> listUsers() => _repository.findAll();

  Future<User> createUser(String email, String name) async {
    final user = User(id: DateTime.now().millisecondsSinceEpoch, email: email, name: name);
    return _repository.save(user);
  }

  Future<bool> deleteUser(int id) => _repository.delete(id);
}
