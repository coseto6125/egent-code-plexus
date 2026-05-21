import Foundation

struct DatabaseConfig {
    var host: String = "localhost"
    var port: Int = 5432
    var name: String = "app"

    var connectionString: String { "postgresql://\(host):\(port)/\(name)" }
}

struct AppConfig {
    var debug: Bool = false
    var secretKey: String = ""
    var database: DatabaseConfig = DatabaseConfig()

    static func load() -> AppConfig {
        var cfg = AppConfig()
        cfg.debug = ProcessInfo.processInfo.environment["DEBUG"] == "true"
        cfg.secretKey = ProcessInfo.processInfo.environment["SECRET_KEY"] ?? "dev-secret"
        return cfg
    }
}
