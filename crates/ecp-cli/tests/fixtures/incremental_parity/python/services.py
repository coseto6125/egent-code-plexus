from typing import List, Optional
from models import User, Post


class UserService:
    def __init__(self, db):
        self.db = db

    def get_user(self, user_id: int) -> Optional[User]:
        return self.db.find_one("users", user_id)

    def list_users(self) -> List[User]:
        return self.db.find_all("users")

    def delete_user(self, user_id: int) -> bool:
        return self.db.delete("users", user_id)


def paginate(items: list, page: int, per_page: int) -> list:
    start = (page - 1) * per_page
    return items[start : start + per_page]
