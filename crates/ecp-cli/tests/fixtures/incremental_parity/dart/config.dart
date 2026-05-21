import 'dart:io';

class DatabaseConfig {
  final String host;
  final int port;
  final String name;

  const DatabaseConfig({
    this.host = 'localhost',
    this.port = 5432,
    this.name = 'app',
  });

  String get connectionString => 'postgresql://$host:$port/$name';
}

class AppConfig {
  final bool debug;
  final String secretKey;
  final DatabaseConfig database;

  const AppConfig({
    this.debug = false,
    this.secretKey = '',
    this.database = const DatabaseConfig(),
  });

  static AppConfig load() {
    return AppConfig(
      debug: Platform.environment['DEBUG'] == 'true',
      secretKey: Platform.environment['SECRET_KEY'] ?? 'dev-secret',
    );
  }
}
