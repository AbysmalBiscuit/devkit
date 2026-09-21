# Cloud setup

The shared cloud bundle lives under `.agents/skills/cloud`. Agents read its skill file directly through a pointer in the generated instructions. Claude settings call the Python scripts under `.agents/skills/cloud/scripts`.

## Provision the VM

Set `CLOUD_AGENT=true` and your `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME`, and `GIT_COMMITTER_EMAIL` in the VM environment. Use an email associated with your GitHub account. Setup requires `GIT_AUTHOR_NAME` to capture the expected commit author.

Set `DEVKIT_INSTALL_DIR` and `MCPLS_INSTALL_DIR` to the prefix each tool installs under, such as `/usr/local`. The binaries land in its `bin` directory, which must be on PATH. Setup and the plugins' bootstrap hooks both install there.

Set `CLOUD_AGENT_TYPE` to the harness running the session so startup can name that harness's own task tracking tools. `claude` is recognized today; any other value, including `codex`, falls back to a generic instruction until that harness's tool names are confirmed.

In the Claude cloud environment's setup-script field, run this from the cloned repository root:

```sh
python3 -B .agents/skills/cloud/scripts/cloud_setup.py --cloud --install
```

The Python entry point reads the devkit and mcpls plugin versions from their repositories' default branches, which is what the plugin marketplaces install. It runs the published installer for each matching release, which requires `sh`, and explicitly installs devkit's command links. mcpls is installed here too, because the `mcpls` plugin's MCP server and hooks run `mcpls` from PATH and the plugin's own bootstrap only installs it after the first session has started.

Setup then writes each plugin bootstrap's `bootstrap-version` stamp under `$XDG_STATE_HOME` (default `~/.local/state`). Hosted setup is cached, so the binaries outlive later plugin releases. The stamp marks them as the bootstrap's own install, and the bootstrap upgrades them when the plugin version moves. Without it the bootstrap records them as external and never replaces them, and a stale devkit then rejects the plugin's newer hook verbs. The commit helper runs from the checkout with `python3`.

The command also generates `devkit.local.toml`, `AGENTS.local.md`, and `CLAUDE.local.md` from the templates in `assets/`. To regenerate these files without downloading binaries, omit `--install`. The generated config points to this checkout's bundled commit helper. Edit `assets/devkit.local.toml` to change the defaults, then rerun setup. This template's `@COMMIT_HELPER@` marker becomes a quoted absolute path during generation.

## Session hooks

- Startup, clear, and fork regenerate the local files, point the agent to `AGENTS.local.md`, name the harness's task tracking tools, and report whether devkit's commands are on PATH.
- Resume and compact inject `references/recovery.md`, and the devkit install command when those commands are missing. They preserve local configuration and progress.
- Set `CLOUD_AGENT=true` in the cloud VM environment for any harness. Without this value, hooks exit silently.

The devkit plugin owns the command and write guards and the project brief. These scripts own cloud workflow instructions and configuration. Claude settings declare devkit and Superpowers as plugins. A config flag only activates enforcement when the devkit plugin's hooks are loaded.

Standing rules belong in `assets/AGENTS.local.md`, which setup copies to the repository root. Workflow selection belongs in `SKILL.md`, reached when starting an issue or feature. Recovery instructions belong in `references/recovery.md`, emitted on resume and compaction. Context that differs between sessions, the harness's task tools and whether devkit is installed, belongs in `scripts/cloud_context.py`, because a copied template cannot vary. Keep each rule in its owning file.

## Cloud commit hook

Setup creates a private hooks directory under Git's metadata directory and selects it with repository-local `core.hooksPath`. It forwards existing hooks and runs the original `commit-msg` hook before checking attribution. Repeated setup preserves the original hook location. The installed hooks and configuration remain outside version control; local sessions do not install them.

With `CLOUD_AGENT=true`, the hook requires the author name captured from `GIT_AUTHOR_NAME` during setup and a `Co-authored-by: Name <email>` trailer. It rejects either missing requirement, including an author changed with `--author`. Supply agent credit through the commit task's `coauthors` argument. Other environments run only the original hooks.

This is a commit-time check, not a security boundary: Git's `--no-verify` and commands that bypass commit hooks can skip it.

The `Setup` hook is an optional CLI entry point for configuration; it runs with explicit Claude initialization flags. It is separate from the hosted environment's setup-script field. Hosted setup is cached, so startup regenerates the checkout-local files even when provisioning was skipped.

## Verification

Run the hook tests from any directory:

```sh
python3 -B /absolute/path/to/repo/.agents/skills/cloud/scripts/cloud.test.py
```

The tests require devkit on PATH and Python with tomllib. They exercise local no-op behavior, config generation in a path containing spaces, settings-based hook invocation, startup context, per-harness task tool naming, devkit reported present and absent, read-only recovery, pinned installs and bootstrap stamps against a `file://` stand-in for GitHub set through `CLOUD_SETUP_GITHUB`, and a patch commit through devkit that preserves unrelated staging. They use temporary directories and perform no downloads.

The patch commit test additionally needs a Git whose `merge-tree` accepts tree arguments, because `git-commit-patch.py` merges the selected and staged trees directly. Git 2.43 rejects that. An older Git skips the test and names the reason instead of failing.

`pyproject.toml` pins the linter and the type checker. Run both from this directory:

```sh
uv run ruff check .
uv run pyrefly check
```

The `cloud` CI job runs the lint, the type check, and the tests on Ubuntu, against a devkit built from the commit under test rather than a published release.

The Claude hook adapter is configured here. A future Codex adapter can call the shared scripts and load the same skill; Codex lifecycle hooks are not configured by these files.

## Sources

- [Claude cloud environments](https://code.claude.com/docs/en/cloud-environments)
- [Claude hooks](https://code.claude.com/docs/en/hooks)
- [Project plugin configuration](https://code.claude.com/docs/en/plugin-marketplaces)
