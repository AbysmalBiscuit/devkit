from users import fetch_user


def handle(request):
    name = fetch_user(request["id"])
    return {"name": name} if name else {"error": "not found"}
