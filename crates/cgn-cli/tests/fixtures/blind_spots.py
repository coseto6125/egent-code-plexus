import importlib

def runtime_eval(user_input):
    return eval(user_input)

def runtime_exec(code):
    exec(code)

def runtime_compile(src):
    return compile(src, "<string>", "exec")

def dynamic_import(name):
    return importlib.import_module(name)

def builtin_import(name):
    return __import__(name)

class Dispatcher:
    def cross_obj_dispatch(self, other, name):
        return getattr(other, name)()  # cross-object reflection

    def self_dispatch(self, name):
        return getattr(self, name)()  # Phase 2 fan-out, NOT blind-spot

    def static_dispatch(self):
        return getattr(self, "handle")()  # static name, NOT blind-spot

def normal_function():
    return 42  # control group, no blind spot
