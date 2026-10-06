# Cloud session content

[harnessup](https://github.com/AbysmalBiscuit/harnessup) prepares Claude Code cloud sessions from this directory. The claude.ai cloud environment that runs them is configured in the claude.ai web UI, not in this repository; recreate it from this file.

Setup script:

```bash
# harnessup main as of <note>
uv tool install git+https://github.com/AbysmalBiscuit/harnessup@main
harnessup setup
```

The script installs harnessup's `main`. claude.ai caches the script's result and reruns it only when the script changes or the cache expires, so a harnessup commit or a change to `manifest.toml` reaches new sessions only after a rebuild. Edit the comment line to rebuild sooner.

Environment variables:

| Variable | Value |
| --- | --- |
| `CLOUD_AGENT` | `true`, so harnessup's session hooks run |
| `UV_TOOL_BIN_DIR`, `DEVKIT_INSTALL_DIR`, `MCPLS_INSTALL_DIR` | Where the setup script installs harnessup, devkit and mcpls: `UV_TOOL_BIN_DIR` and the `bin` directory of each install prefix, such as `/usr/local`, must be on `PATH` |
| `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL` | The identity of the session's commits, with an email GitHub associates with the account |

Network access must reach GitHub, for harnessup's source, the plugin marketplaces and the binaries the plugin bootstraps download, and PyPI, for harnessup's dependencies.

`harnessup setup` writes its report to `~/.local/state/harnessup/setup.json`. A session reports failed items at startup.
