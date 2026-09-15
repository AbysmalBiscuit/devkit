"""Recover cloud work after compaction or session resume."""

from cloud_setup import SKILL, in_cloud


def main():
    if in_cloud():
        print((SKILL / "references/recovery.md").read_text(encoding="utf-8"), end="")


if __name__ == "__main__":
    main()
