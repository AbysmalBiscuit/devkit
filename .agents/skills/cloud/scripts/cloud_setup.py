"""Install cloud tooling on request and generate checkout-local preferences."""

import argparse
import json
import os
import subprocess
import urllib.request
from pathlib import Path

SKILL = Path(__file__).resolve().parents[1]
ROOT = SKILL.parents[2]


def in_cloud():
    return os.environ.get("CLOUD_AGENT") == "true"


def configure():
    helper = Path(__file__).resolve().with_name("git-commit-patch.py")
    template = (SKILL / "assets/devkit.local.toml").read_text(encoding="utf-8")
    config = template.replace("@COMMIT_HELPER@", json.dumps(str(helper)))
    (ROOT / "devkit.local.toml").write_text(config, encoding="utf-8")
    for name in ("AGENTS.local.md", "CLAUDE.local.md"):
        (ROOT / name).write_text((SKILL / "assets" / name).read_text(encoding="utf-8"), encoding="utf-8")


def install_devkit():
    url = "https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.sh"
    with urllib.request.urlopen(url, timeout=30) as response:
        installer = response.read()
    env = dict(os.environ, CARGO_DIST_FORCE_INSTALL_DIR="/usr/local")
    subprocess.run(["sh"], input=installer, env=env, check=True, timeout=240)
    subprocess.run(["/usr/local/bin/devkit", "install-links"], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cloud",
        action="store_true",
        help="Configure before the cloud environment flag is available",
    )
    parser.add_argument(
        "--install",
        action="store_true",
        help="Install the latest devkit release into /usr/local/bin",
    )
    parser.add_argument(
        "--handoff", action="store_true", help="Print the final handoff message for the agent"
    )
    args = parser.parse_args()
    if not (args.cloud or in_cloud()):
        return
    if args.install:
        install_devkit()
    configure()
    if args.handoff:
        print(f"Cloud configuration generated in {ROOT}. Read {ROOT / 'AGENTS.local.md'} before continuing the task.")


if __name__ == "__main__":
    main()
