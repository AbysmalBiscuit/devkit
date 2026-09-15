# Cloud setup

The shared cloud bundle lives under `.agents/skills/cloud`. Agents read its skill file directly through a pointer in the generated instructions. Claude settings call the Python scripts under `.agents/skills/cloud/scripts`.

## Provision the VM

In the Claude cloud environment's setup-script field, run this from the cloned repository root:

```sh
python3 -B .agents/skills/cloud/scripts/cloud_setup.py --cloud --install
```

The Python entry point downloads and runs devkit's published installer, which requires `sh`, and explicitly installs its command links. The binaries land in `/usr/local/bin`; this install requires the root account supplied by Claude's hosted setup environment. The commit helper runs from the checkout with `python3`.

The command also generates `devkit.local.toml`, `AGENTS.local.md`, and `CLAUDE.local.md` from the templates in `assets/`. To regenerate these files without downloading binaries, omit `--install`. The generated config points to this checkout's bundled commit helper. Edit `assets/devkit.local.toml` to change the defaults, then rerun setup. This template's `@COMMIT_HELPER@` marker becomes a quoted absolute path during generation.

## Session hooks

- Startup, clear, and fork regenerate the local files and point the agent to `AGENTS.local.md`.
- Resume and compact inject only `references/recovery.md`. They preserve local configuration and progress.
- Set `CLOUD_AGENT=true` in the cloud VM environment for any harness. Without this value, hooks exit silently.

The devkit plugin owns the command and write guards and the project brief. These scripts own cloud workflow instructions and configuration. Claude settings declare devkit and Superpowers as plugins. A config flag only activates enforcement when the devkit plugin's hooks are loaded.

Standing rules belong in `assets/AGENTS.local.md`, which setup copies to the repository root. Workflow selection belongs in `SKILL.md`, reached when starting an issue or feature. Recovery instructions belong in `references/recovery.md`, emitted on resume and compaction. Keep each rule in its owning file.

The `Setup` hook is an optional CLI entry point for configuration; it runs with explicit Claude initialization flags. It is separate from the hosted environment's setup-script field. Hosted setup is cached, so startup regenerates the checkout-local files even when provisioning was skipped.

## Verification

Run the hook tests from any directory:

```sh
python3 -B /absolute/path/to/repo/.agents/skills/cloud/scripts/cloud.test.py
```

The tests require devkit on PATH and Python with tomllib. They exercise local no-op behavior, config generation in a path containing spaces, settings-based hook invocation, startup context, read-only recovery, and a patch commit through devkit that preserves unrelated staging. They use temporary directories and perform no downloads.

The Claude hook adapter is configured here. A future Codex adapter can call the shared scripts and load the same skill; Codex lifecycle hooks are not configured by these files.

## Sources

- [Claude cloud environments](https://code.claude.com/docs/en/cloud-environments)
- [Claude hooks](https://code.claude.com/docs/en/hooks)
- [Project plugin configuration](https://code.claude.com/docs/en/plugin-marketplaces)
