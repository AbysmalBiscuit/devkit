USERS = {1: "ada", 2: "grace"}


def fetch_user(user_id):
    return USERS.get(user_id)
