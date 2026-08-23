import json


def load_config(path):
    try:
        return json.loads(open(path).read())
    except OSError:
        pass
    return {}
