using System;

namespace Example
{
    public class DatabaseConfig
    {
        public string Host { get; set; } = "localhost";
        public int Port { get; set; } = 5432;
        public string Name { get; set; } = "app";

        public string ConnectionString() => $"Host={Host};Port={Port};Database={Name}";
    }

    public class AppConfig
    {
        public bool Debug { get; set; }
        public string SecretKey { get; set; } = string.Empty;
        public DatabaseConfig Database { get; set; } = new();

        public static AppConfig Load() => new()
        {
            Debug = Environment.GetEnvironmentVariable("DEBUG") == "true",
            SecretKey = Environment.GetEnvironmentVariable("SECRET_KEY") ?? "dev-secret",
        };
    }
}
