# Committing: `devkit commit`

`devkit commit` records one selection with a message the project's `commit_message` template renders, and leaves the working tree and every other session's staged changes as they were. It stages nothing you did not name, so commit with it rather than `git add` and `git commit` in a shared checkout.

```sh
devkit commit --files src/a.rs src/b.rs --subject 'fix(scope): imperative summary' --coauthor 'Model <email>'
devkit commit --patch hunks.patch --subject '...' --body 'Why, when the subject does not say.'
devkit commit --amend --subject '...'               # a new message for the last commit
```

- `--files` commits each path as the working tree has it, new and deleted files included.
- `--patch` commits part of a file. Write only the hunks to commit as a patch against HEAD (`git diff -- path > hunks.patch`, then trim it); the working tree is not read. A patch whose hunks overlap changes already staged is refused, as is one touching a path whose merge driver could not keep those staged hunks exact.
- `--amend` changes only the message; staged changes stay staged.
- The message is the subject, then `--body` after a blank line, then a `Co-authored-by` trailer per `--coauthor`. `devkit template show commit_message` prints the project's template and any `--arg` it reads; a project can require `--coauthor` of agents.
- Commit hooks and signing run as they do for `git commit`. A refused or failed commit leaves HEAD and the index unchanged, and says why. With `--patch`, a commit hook that changes the committed tree is refused too.
- When the output says recovery files were retained, the new HEAD could not be confirmed: inspect `git log` before retrying, and do not run the commit again blind.
