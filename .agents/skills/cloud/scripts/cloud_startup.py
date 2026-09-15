"""Generate cloud preferences and point the session to its standing rules."""

from cloud_context import devkit_report, task_tools
from cloud_setup import ROOT, configure, in_cloud


def main():
    if not in_cloud():
        return
    configure()
    print(f"Read {ROOT / 'AGENTS.local.md'} before continuing the task.")
    print(task_tools())
    print(devkit_report())


if __name__ == "__main__":
    main()
