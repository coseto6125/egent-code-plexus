from dataclasses import dataclass
from typing import Optional


@dataclass
class User:
    id: int
    email: str
    role: str = "user"


@dataclass
class Post:
    id: int
    title: str
    body: str
    author_id: int


def create_user(email: str, role: str = "user") -> User:
    return User(id=0, email=email, role=role)


def validate_email(email: str) -> bool:
    return "@" in email and "." in email.split("@")[1]
