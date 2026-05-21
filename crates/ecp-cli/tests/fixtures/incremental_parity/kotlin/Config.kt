package com.example

data class DatabaseConfig(
    val host: String = "localhost",
    val port: Int = 5432,
    val name: String = "app"
)

data class AppConfig(
    val debug: Boolean = false,
    val secretKey: String = "",
    val database: DatabaseConfig = DatabaseConfig()
)

fun loadConfig(): AppConfig {
    return AppConfig(
        debug = System.getenv("DEBUG")?.lowercase() == "true",
        secretKey = System.getenv("SECRET_KEY") ?: "dev-secret"
    )
}
