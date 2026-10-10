# Rendering templates: `devkit template`

devkit renders; you deliver. Reach for `devkit template render` when text has to reach a tool devkit does not send to (a Linear status update, a Jira comment, a release note, an incident post) and the project keeps a template for it, to name a branch or worktree the way the project's `workspace setup` would, or to read a PR body or Slack message before the command that sends it runs. Upload the output through whatever you already have: an MCP server, `gh`, a browser.

```sh
devkit template list                                 # custom and built-in templates, and the args each reads
devkit template show <name>                          # the source, then each arg: who must pass it, default, description
devkit template render <name> --arg k=v              # the rendered text on stdout
devkit template render <name> --arg-file k=notes.md  # a multi-line value from a file, or k=- for stdin
```

- `render` prints the text byte for byte, so `> file` or a pipe gets it unchanged. `--json` on all three verbs emits JSON instead; `render --json` is `{"text": ...}`.
- A required arg left out is refused before anything renders, naming each `--arg` and its description the way `devrun task` does. An `--arg` the template never reads is refused too.
- Every template sees `prefix` (`defaults.branch_prefix`), `branch`, and `issue`, `slug` and `apps` from the worktree's `.devkit/issue.toml`. Any other name it reads is an arg, including those the checkout lacks: outside a workspace, `render branch --arg slug=add-login` names a new branch. In one, `render pr_body --arg input=...` prints the body `devkit pr create --pr-body ...` would send.
- The built-ins are every template `workspace`, `ticket` and `devkit pr` render, `branch`, `worktree_dir`, `checkout_worktree_dir`, `issue_summary_path`, `issue_summary`, `pr_title`, `pr_body`, `issue_title`, `issue_body`, `review_request` and `review_finish`, plus `commit_message`, which `devkit commit` renders. `render` applies no length limit, so `branch_max` and the other `*_max` settings do not shorten its output, and `short_slug` is an arg.
- A project's own templates live under `[templates.custom.<name>]`, with a `body` and a `description`; `devkit schema` has the details. A custom template wins over a built-in of the same name.
- MCP has the same three as `templates.list`, `templates.show` and `templates.render`, with `args` as a JSON object, so a multi-line value needs no file. `templates.render` takes the `commit_message` parts `templates.show` names by flag as parameters of the same name, `subject`, `body` and a `coauthor` array, not inside `args`.
