def unrelated():
    try:
        return compute()
    except TypeError:
        pass
    return 0


def compute():
    return 1
