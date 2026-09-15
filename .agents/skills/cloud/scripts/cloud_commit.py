"""Validate cloud commit attribution after the existing commit-msg hook."""

import os
import re
import subprocess
import sys
from pathlib import Path


def main():
    expected, original, message = sys.argv[1:]
    if os.access(original, os.X_OK):
        result = subprocess.run([original, message])
        if result.returncode:
            return result.returncode
    if os.environ.get("CLOUD_AGENT") != "true":
        return 0
    identity = subprocess.check_output(["git", "var", "GIT_AUTHOR_IDENT"], text=True)
    author = identity.rsplit(" <", 1)[0]
    if author != expected:
        print(f"Cloud commit refused: author must be {expected!r}; got {author!r}.", file=sys.stderr)
        return 1
    trailers = subprocess.check_output(
        ["git", "interpret-trailers", "--parse"], input=Path(message).read_bytes()
    ).decode("utf-8", errors="replace")
    if not any(re.fullmatch(r"Co-authored-by:\s+[^<>\n]+\s+<[^<>\s]+@[^<>\s]+>\s*", line, re.IGNORECASE)
               for line in trailers.splitlines()):
        print(
            "Cloud commit refused: add Co-authored-by: Agent Name <email> "
            "via --arg coauthors='Agent Name <email>'.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
