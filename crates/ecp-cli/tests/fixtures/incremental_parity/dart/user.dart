class User {
  final int id;
  final String email;
  final String name;
  final String role;

  const User({
    required this.id,
    required this.email,
    required this.name,
    this.role = 'user',
  });

  bool get isAdmin => role == 'admin';
  String get displayName => '$name <$email>';

  User copyWith({String? email, String? name, String? role}) => User(
        id: id,
        email: email ?? this.email,
        name: name ?? this.name,
        role: role ?? this.role,
      );
}
