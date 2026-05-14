from rename_def import old_name


def use_it():
    return old_name()


def use_it_twice():
    old_name()
    return old_name()
