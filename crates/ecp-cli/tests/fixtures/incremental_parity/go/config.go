package main

import "os"

type Config struct {
	Debug     bool
	SecretKey string
	DBHost    string
	DBPort    string
}

func LoadConfig() Config {
	return Config{
		Debug:     os.Getenv("DEBUG") == "true",
		SecretKey: envOr("SECRET_KEY", "dev-secret"),
		DBHost:    envOr("DB_HOST", "localhost"),
		DBPort:    envOr("DB_PORT", "5432"),
	}
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
