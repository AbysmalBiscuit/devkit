#!/usr/bin/env python3
"""Commit a HEAD-based patch while preserving unrelated staged changes."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path

REDIRECTING_ENV = {
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CEILING_DIRECTORIES",
}
OPERATION_PATHS = (
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
)


class CommitError(Exception):
    def __init__(self, message: str, status: int = 1) -> None:
        super().__init__(message)
        self.status = status


@dataclass
class Git:
    cwd: Path
    env: dict[str, str]

    def run(
        self,
        *args: str,
        index: Path | None = None,
        hooks: Path | None = None,
        data: bytes | None = None,
        check: bool = True,
    ) -> subprocess.CompletedProcess[bytes]:
        env = self.env.copy()
        if index is not None:
            env["GIT_INDEX_FILE"] = str(index)
        argv = ["git", "-C", str(self.cwd)]
        if hooks is not None:
            argv.extend(["-c", f"core.hooksPath={hooks.as_posix()}"])
        result = subprocess.run(
            [*argv, *args], input=data, env=env, capture_output=True, check=False
        )
        if check and result.returncode:
            detail = (result.stderr or result.stdout).decode(errors="replace").strip()
            raise CommitError(f"git {args[0]}: {detail}", result.returncode)
        return result

    def text(self, *args: str, index: Path | None = None) -> str:
        return self.run(*args, index=index).stdout.decode().strip()

    def path(self, name: str) -> Path:
        return Path(self.text("rev-parse", "--path-format=absolute", "--git-path", name))


@dataclass
class HookState:
    original_hooks: str
    old_head: str | None
    target_ref: str
    selected_tree: str
    proposed: str


def write_synced(path: Path, data: bytes) -> None:
    with path.open("wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())


def prepare_hooks(git: Git, scratch: Path, old_head: str | None, target: str, tree: str) -> Path:
    original = git.path("hooks")
    state = HookState(str(original), old_head, target, tree, str(scratch / "proposed"))
    state_path = scratch / "hook-state.json"
    write_synced(state_path, json.dumps(asdict(state)).encode())
    hooks = scratch / "hooks"
    hooks.mkdir()
    names = {"reference-transaction"}
    if original.is_dir():
        names.update(path.name for path in original.iterdir() if path.is_file())
    for name in names:
        command = shlex.join(
            [
                Path(sys.executable).as_posix(),
                Path(__file__).resolve().as_posix(),
                "--run-hook",
                state_path.as_posix(),
                name,
            ]
        )
        wrapper = hooks / name
        wrapper.write_text(f'#!/bin/sh\nexec {command} "$@"\n', encoding="utf-8", newline="\n")
        wrapper.chmod(0o700)
    return hooks


def run_hook(state_path: Path, event: str, args: list[str]) -> int:
    state = HookState(**json.loads(state_path.read_text()))
    payload = sys.stdin.buffer.read() if event == "reference-transaction" else None
    argv = [
        "git",
        "-c",
        f"core.hooksPath={Path(state.original_hooks).as_posix()}",
        "hook",
        "run",
        "--ignore-missing",
    ]
    with tempfile.TemporaryDirectory(dir=state_path.parent) as input_dir:
        if payload is not None:
            input_path = Path(input_dir) / "stdin"
            input_path.write_bytes(payload)
            argv.extend(["--to-stdin", str(input_path)])
        result = subprocess.run([*argv, event, "--", *args], check=False)
    if result.returncode or event != "reference-transaction" or args != ["prepared"]:
        return result.returncode
    git = Git(Path.cwd(), dict(os.environ))
    symbolic = git.run("symbolic-ref", "-q", "HEAD", check=False)
    target = symbolic.stdout.decode().strip() if symbolic.returncode == 0 else "HEAD"
    if target != state.target_ref:
        raise CommitError("HEAD reference changed while preparing the patch commit")
    matched = False
    for line in (payload or b"").decode().splitlines():
        old, new, ref = line.split(" ", 2)
        if ref not in {state.target_ref, "HEAD"}:
            continue
        matched = True
        if old != (state.old_head or "0" * len(new)):
            raise CommitError("HEAD changed while preparing the patch commit")
        if git.text("rev-parse", f"{new}^{{tree}}") != state.selected_tree:
            raise CommitError("a commit hook changed the selected tree; commit refused")
        write_synced(Path(state.proposed), new.encode())
    if not matched:
        proposed = Path(state.proposed)
        if not proposed.exists() or git.text("rev-parse", "HEAD") != proposed.read_text():
            raise CommitError("reference update does not target the expected HEAD; commit refused")
    return 0


def check_merge_drivers(git: Git, base: str, selected: str, prior: str, empty_index: Path) -> None:
    changed = [
        set(
            git.run(
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-r",
                "-z",
                base,
                tree,
            ).stdout.split(b"\0")
        )
        for tree in (selected, prior)
    ]
    overlap = (changed[0] & changed[1]) - {b""}
    if not overlap:
        return
    attributes = git.run(
        "check-attr",
        "-z",
        "--stdin",
        "merge",
        data=b"\0".join(sorted(overlap)) + b"\0",
        index=empty_index,
    ).stdout.split(b"\0")
    for offset in range(0, len(attributes) - 1, 3):
        path, _, value = attributes[offset : offset + 3]
        if value in {b"set", b"unset"}:
            continue
        if value == b"unspecified":
            default = git.run("config", "--get", "merge.default", check=False)
            value = default.stdout.strip() if default.returncode == 0 else b"text"
        driver = os.fsdecode(value)
        custom = git.run("config", "--get-regexp", f"^merge\\.{driver}\\.", check=False)
        if driver not in {"text", "binary"} or custom.returncode == 0:
            raise CommitError(
                f"{os.fsdecode(path)}: merge driver {driver!r} cannot preserve exact staged hunks"
            )


def commit_patch(patch: Path, message: str) -> int:
    env = {key: value for key, value in os.environ.items() if key not in REDIRECTING_ENV}
    git = Git(Path.cwd(), env)
    git.cwd = Path(git.text("rev-parse", "--show-toplevel"))
    index = git.path("index")
    lock = index.with_name(index.name + ".lock")
    try:
        descriptor = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as error:
        raise CommitError(f"{lock}: another Git operation holds the index lock") from error
    os.close(descriptor)
    scratch: Path | None = None
    retain = False
    owns_lock = True
    try:
        for name in OPERATION_PATHS:
            if git.path(name).exists():
                raise CommitError(f"{name}: finish the existing Git operation first")
        if git.run("ls-files", "--unmerged", "-z").stdout:
            raise CommitError("index contains unresolved merge entries")
        scratch = Path(tempfile.mkdtemp(prefix="commit-patch-", dir=index.parent))
        original = scratch / "original.index"
        prior = scratch / "prior.index"
        candidate = scratch / "candidate.index"
        replacement = scratch / "replacement.index"
        empty_hooks = scratch / "empty-hooks"
        empty_hooks.mkdir()
        head = git.run("rev-parse", "--verify", "HEAD", check=False)
        old_head = head.stdout.decode().strip() if head.returncode == 0 else None
        symbolic = git.run("symbolic-ref", "-q", "HEAD", check=False)
        target = symbolic.stdout.decode().strip() if symbolic.returncode == 0 else "HEAD"
        if old_head is None and target == "HEAD":
            raise CommitError("HEAD cannot be resolved to a commit or an unborn branch")
        if index.exists():
            shutil.copyfile(index, original)
        else:
            git.run("read-tree", "--empty", index=original, hooks=empty_hooks)
        shutil.copyfile(original, prior)
        prior_tree = git.text("write-tree", index=prior)
        git.run("read-tree", old_head or "--empty", index=candidate, hooks=empty_hooks)
        base_tree = git.text("write-tree", index=candidate)
        git.run(
            "apply",
            "--cached",
            "--whitespace=nowarn",
            "--",
            str(patch),
            index=candidate,
            hooks=empty_hooks,
        )
        selected_tree = git.text("write-tree", index=candidate)
        if selected_tree == base_tree:
            raise CommitError("patch contains no changes relative to HEAD")
        check_merge_drivers(git, base_tree, selected_tree, prior_tree, scratch / "attributes.index")
        merged = git.run(
            "-c",
            "merge.renames=false",
            "-c",
            "merge.renormalize=false",
            "merge-tree",
            "--write-tree",
            f"--merge-base={base_tree}",
            selected_tree,
            prior_tree,
            check=False,
        )
        if merged.returncode:
            raise CommitError(
                "patch overlaps staged changes; HEAD and the shared index are unchanged\n"
                + (merged.stderr + merged.stdout).decode(errors="replace"),
                merged.returncode,
            )
        merged_tree = merged.stdout.decode().splitlines()[0]
        shutil.copyfile(original, replacement)
        git.run(
            "read-tree",
            "-i",
            "-m",
            prior_tree,
            merged_tree,
            index=replacement,
            hooks=empty_hooks,
        )
        hooks = prepare_hooks(git, scratch, old_head, target, selected_tree)
        retain = True
        result = git.run("commit", "-m", message, index=candidate, hooks=hooks, check=False)
        sys.stdout.buffer.write(result.stdout)
        sys.stderr.buffer.write(result.stderr)
        proposed_path = scratch / "proposed"
        proposed = proposed_path.read_text() if proposed_path.exists() else None
        observed = git.run("rev-parse", "--verify", "HEAD", check=False)
        new_head = observed.stdout.decode().strip() if observed.returncode == 0 else None
        if proposed is None or new_head != proposed:
            if proposed is not None or new_head != old_head or result.returncode == 0:
                raise CommitError(
                    f"HEAD could not be confirmed after git commit; retain {scratch} and {lock} "
                    "for recovery. Do not retry the commit."
                )
            retain = False
            return result.returncode
        try:
            shutil.copyfile(replacement, lock)
            with lock.open("rb") as handle:
                os.fsync(handle.fileno())
            os.replace(lock, index)
            owns_lock = False
            retain = False
        except OSError as error:
            raise CommitError(
                f"commit {new_head} was created, but the index could not be installed: {error}\n"
                f"original index: {original}\nreplacement index: {replacement}\n"
                f"owned index lock: {lock}\nDo not retry the commit; recover the index first."
            ) from error
        if result.returncode:
            print(
                f"commit {new_head} was created and the preserved index installed, "
                f"but Git exited with status {result.returncode}; do not retry the commit.",
                file=sys.stderr,
            )
        return result.returncode
    finally:
        if retain:
            print(
                f"recovery files retained at {scratch}; index lock: {lock}. "
                "Inspect HEAD before retrying.",
                file=sys.stderr,
            )
        else:
            if owns_lock:
                lock.unlink(missing_ok=True)
            if scratch is not None:
                shutil.rmtree(scratch)


def main() -> int:
    if sys.argv[1:2] == ["--run-hook"]:
        try:
            return run_hook(Path(sys.argv[2]), sys.argv[3], sys.argv[4:])
        except (CommitError, OSError, ValueError) as error:
            print(f"git-commit-patch: {error}", file=sys.stderr)
            return 1
    parser = argparse.ArgumentParser(
        description="Commit a patch based on HEAD, preserving unrelated staged changes.",
        epilog=(
            "The patch is applied to a private index, never to working-tree files. "
            "Conflicting staged hunks and custom or union merge drivers on files changed "
            "on both sides are refused. Hooks and signing run normally. If publication "
            "or index installation is uncertain, recovery files and the index lock are "
            "retained; inspect HEAD before retrying. HEAD and index updates are not "
            "atomic across crashes."
        ),
    )
    parser.add_argument("--patch", required=True, type=Path)
    parser.add_argument("--message", required=True)
    args = parser.parse_args()
    try:
        return commit_patch(args.patch.resolve(strict=True), args.message)
    except (CommitError, OSError) as error:
        print(f"git-commit-patch: {error}", file=sys.stderr)
        return error.status if isinstance(error, CommitError) else 1


if __name__ == "__main__":
    sys.exit(main())
