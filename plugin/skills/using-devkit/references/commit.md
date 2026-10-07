# Committing: `devkit commit`

`devkit commit` records one selection with a message the project's `commit_message` template renders, and leaves the working tree and every other session's staged changes as they were. It stages nothing you did not name, so commit with it rather than `git add` and `git commit` in a shared checkout.

```sh
devkit commit --files src/a.rs src/b.rs --subject 'fix(scope): imperative summary' --coauthor 'Model <email>'
devkit commit --patch hunks.patch --subject '...' --body 'Why, when the subject does not say.'
devkit commit --amend --subject '...'               # a new message for the last commit
```

- To commit part of a file, write only the hunks to commit as a patch against HEAD (`git diff -- path > hunks.patch`, then trim it) and pass it to `--patch`. A patch touching a path whose merge driver could not keep the staged hunks exact is refused.
- The message is the subject, then `--body` after a blank line, then a `Co-authored-by` trailer per `--coauthor`. `devkit template show commit_message` prints the project's template, each message part by its `devkit commit` flag with who must pass it, and any `--arg` it reads. Only `--subject` is required unless the project requires another part, as a project can require `--coauthor` of agents. `devkit template render commit_message` previews the message, taking `--subject`, `--body` and `--coauthor` as `devkit commit` does, and needs no more than it does.
- Commit hooks and signing run as they do for `git commit`. A refused or failed commit leaves HEAD and the index unchanged, and says why. With `--files` or `--patch`, a commit hook that changes the committed tree is refused too.
- When the output says recovery files were retained, the new HEAD could not be confirmed: inspect `git log` before retrying, and do not run the commit again blind.
