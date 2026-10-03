import sys

from users import fetch_user


def main():
    print(fetch_user(int(sys.argv[1])) or "no such user")


if __name__ == "__main__":
    main()
