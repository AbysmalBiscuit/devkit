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


def install_commit_hook():
    expected = os.environ.get("GIT_AUTHOR_NAME", "").strip()
    if not expected:
        raise RuntimeError("Set GIT_AUTHOR_NAME in the cloud VM environment before setup.")

    def git(*args):
        return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True).strip()

    hooks = Path(git("rev-parse", "--absolute-git-dir")) / "devkit-cloud-hooks"
    original = Path(git("rev-parse", "--path-format=absolute", "--git-path", "hooks"))
    source = hooks / "original-path.json"
    if original == hooks:
        original = Path(json.loads(source.read_text(encoding="utf-8")))
    hooks.mkdir(exist_ok=True)
    source.write_text(json.dumps(str(original)), encoding="utf-8")
    if original.is_dir():
        for entry in original.iterdir():
            target = hooks / entry.name
            if entry.name != "commit-msg" and not target.exists() and not target.is_symlink():
                target.symlink_to(entry)
    script = Path(__file__).with_name("cloud_commit.py")
    command = ["python3", str(script), expected, str(original / "commit-msg")]
    wrapper = hooks / "commit-msg"
    wrapper.write_text(
        "#!/usr/bin/env python3\nimport os\nimport sys\n"
        f"command = {command!r} + sys.argv[1:]\n"
        "os.execvp(command[0], command)\n",
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    git("config", "--local", "core.hooksPath", str(hooks))


def configure():
    install_commit_hook()
    helper = Path(__file__).resolve().with_name("git-commit-patch.py")
    template = (SKILL / "assets/devkit.local.toml").read_text(encoding="utf-8")
    config = template.replace("@COMMIT_HELPER@", json.dumps(str(helper)))
    (ROOT / "devkit.local.toml").write_text(config, encoding="utf-8")
    for name in ("AGENTS.local.md", "CLAUDE.local.md"):
        (ROOT / name).write_text((SKILL / "assets" / name).read_text(encoding="utf-8"), encoding="utf-8")


def run_installer(app):
    url = f"https://github.com/AbysmalBiscuit/{app}/releases/latest/download/{app}-installer.sh"
    with urllib.request.urlopen(url, timeout=30) as response:
        installer = response.read()
    env = dict(os.environ, CARGO_DIST_FORCE_INSTALL_DIR="/usr/local")
    subprocess.run(["sh"], input=installer, env=env, check=True, timeout=240)


def install_tools():
    run_installer("devkit")
    subprocess.run(["/usr/local/bin/devkit", "install-links"], check=True)
    run_installer("mcpls")


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
        help="Install the latest devkit and mcpls releases into /usr/local/bin",
    )
    parser.add_argument(
        "--handoff", action="store_true", help="Print the final handoff message for the agent"
    )
    args = parser.parse_args()
    if not (args.cloud or in_cloud()):
        return
    if args.install:
        install_tools()
    configure()
    if args.handoff:
        print(f"Cloud configuration generated in {ROOT}. Read {ROOT / 'AGENTS.local.md'} before continuing the task.")


if __name__ == "__main__":
    main()
