"""Describe the tooling a cloud session actually has, for its startup context."""

import os
import shutil
import subprocess

# Each harness names its own task tracking tools. A harness absent from this
# table gets the generic sentence rather than a guess, because naming a tool
# that does not exist costs the agent a turn discovering that. Codex is absent
# until its tool names are confirmed against the harness.
TASK_TOOLS = {"claude": ("TaskCreate", "TaskUpdate", "TaskList", "TaskGet")}

GENERIC_TASK_TOOLS = "Use your task tracking tools to track progress if they are available."

# `devkit install-links` is a separate step from the install, so the binary can
# be present with the links absent. The command guard's advice to run `issue
# setup` fails on exactly that, so every name is checked, not just `devkit`.
COMMANDS = ("devkit", "issue", "devrun", "portm", "lockm", "docm", "devkit-mcp")

INSTALL = "python3 -B .agents/skills/cloud/scripts/cloud_setup.py --cloud --install"


def task_tools():
    """Tell the session to track progress where the person watching can see it."""
    tools = TASK_TOOLS.get(os.environ.get("CLOUD_AGENT_TYPE", "").strip().lower())
    if not tools:
        return GENERIC_TASK_TOOLS
    return (
        f"Track progress with {', '.join(tools[:-1])} and {tools[-1]}, alongside the "
        "workflow's progress ledger, so the work stays visible while the session runs."
    )


def devkit_version():
    """The installed version, or None when the binary does not run."""
    try:
        done = subprocess.run(["devkit", "--version"], capture_output=True, text=True, timeout=2)
    except (OSError, subprocess.SubprocessError):
        return None
    fields = done.stdout.split()
    return fields[-1] if done.returncode == 0 and fields else None


def devkit_state():
    """`(version, missing)`, where a version of None means devkit is unusable.

    Resolving a name on PATH says a file is there, not that it runs, so the
    version doubles as the executability check: a partial install can leave a
    command link whose target is gone.
    """
    missing = [name for name in COMMANDS if shutil.which(name) is None]
    return (None if missing else devkit_version()), missing


def devkit_advice(state):
    """Install advice when `state` reports devkit unusable, else None."""
    version, missing = state
    if version:
        return None
    detail = f"{', '.join(missing)} missing from PATH" if missing else "devkit does not run"
    return (
        f"devkit is not usable here: {detail}. "
        f"Install it with `{INSTALL}` before reaching for its commands."
    )


def devkit_report():
    """Startup context naming devkit's state, whether it is present or not.

    Hosted setup is cached, so a session starts with CLOUD_AGENT set and no
    devkit often enough to matter. An agent that is not told reads the absence
    as a broken MCP server or a mistyped command rather than a missing install.
    """
    state = devkit_state()
    version, _ = state
    return devkit_advice(state) or f"devkit {version} is on PATH, with {', '.join(COMMANDS[1:])}."
