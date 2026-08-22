from pattern_def import load_config


def read_settings():
    try:
        return load_config("settings.json")
    except ValueError:
        pass
    return None
