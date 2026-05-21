import os
from dataclasses import dataclass, field
from typing import List


@dataclass
class DatabaseConfig:
    host: str = "localhost"
    port: int = 5432
    name: str = "app"


@dataclass
class AppConfig:
    debug: bool = False
    secret_key: str = ""
    allowed_hosts: List[str] = field(default_factory=list)
    database: DatabaseConfig = field(default_factory=DatabaseConfig)


def load_config() -> AppConfig:
    return AppConfig(
        debug=os.getenv("DEBUG", "false").lower() == "true",
        secret_key=os.getenv("SECRET_KEY", "dev-secret"),
    )
