class Dispatcher:
    def __init__(self):
        pass

    def dispatch(self, action: str, data):
        method_name = f"handle_{action}"
        return getattr(self, method_name)(data)

    def alt_dispatch(self, name: str):
        return getattr(self, name, self.fallback)()

    def handle_create(self, data):
        return "created"

    def handle_delete(self, data):
        return "deleted"

    def handle_update(self, data):
        return "updated"

    def fallback(self):
        return "fallback"

    # Negative: 字串字面值不該觸發 fan-out
    def static_dispatch(self):
        return getattr(self, "handle_create")()

    # Negative: 沒有 call (())，只是 getattr 取值，不該觸發
    def get_method(self, name):
        return getattr(self, name)

    # Negative: 跨物件 getattr，不該觸發
    def cross_obj(self, other, name):
        return getattr(other, name)()
