"""Recover cloud work after compaction or session resume."""

from cloud_context import devkit_advice, devkit_state
from cloud_setup import SKILL, in_cloud


def main():
    if not in_cloud():
        return
    print((SKILL / "references/recovery.md").read_text(encoding="utf-8"), end="")
    # Recovery is deliberately short, so this line appears only when it is
    # actionable. A session that lost the startup context to compaction would
    # otherwise rediscover a missing devkit through a failing command.
    advice = devkit_advice(devkit_state())
    if advice:
        print(advice)


if __name__ == "__main__":
    main()
