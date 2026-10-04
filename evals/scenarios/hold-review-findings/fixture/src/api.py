USERS = {1: "ada", 2: "grace", 3: "linus"}


def _tmp(page, size):
    start = page * size
    return start, start + size


def page_slice(items, page, size):
    """Page `page` of `items`, `size` items per page. Pages count from 1."""
    start, end = _tmp(page, size)
    return items[start:end]


def get_user(user_id):
    return USERS.get(user_id)


def get_user_v1(user_id):
    """Deprecated alias of get_user, kept for callers of the v1 API."""
    return get_user(user_id)
