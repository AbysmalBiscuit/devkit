"""Load the cloud workflow at startup or after clearing context."""

from cloud_setup import SKILL, configure, in_cloud


def main():
    if not in_cloud():
        return
    configure()
    skill = (SKILL / "SKILL.md").read_text(encoding="utf-8")
    print(skill.split("---", 2)[2].lstrip())


if __name__ == "__main__":
    main()
