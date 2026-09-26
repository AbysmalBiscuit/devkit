"""Print each file's o200k_base token count, a close proxy for Claude's."""

import sys

import tiktoken

encoding = tiktoken.get_encoding("o200k_base")
for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as f:
        print(len(encoding.encode(f.read())))
