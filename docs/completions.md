# Shell completions

`devkit` and every old name except `devkit-mcp`, which takes no subcommands at all, generate their own completion script via a `completions <shell>` subcommand. Supported shells are bash, zsh, fish, elvish, nushell, and powershell, and each script completes its own subcommands.

```sh
devkit completions zsh  > ~/.zfunc/_devkit
devrun completions zsh  > ~/.zfunc/_devrun
docm completions zsh    > ~/.zfunc/_docm
issue completions zsh   > ~/.zfunc/_issue
lockm completions zsh   > ~/.zfunc/_lockm
portm completions zsh   > ~/.zfunc/_portm
# bash:
issue completions bash > ~/.local/share/bash-completion/completions/issue
```

A nushell script defines a module and re-exports it, so save it to a file and `source` that file from `config.nu` rather than piping it in:

```nu
mkdir ($nu.default-config-dir | path join completions)
issue completions nushell | save -f ($nu.default-config-dir | path join completions issue.nu)
# then in config.nu:
source ($nu.default-config-dir | path join completions issue.nu)
```

## One file for every name

`devkit completions --all <shell>` writes all of the above concatenated, `devkit` first and then each old name, so a dotfile manager regenerates every completion in one command:

```sh
devkit completions --all fish > ~/.config/fish/completions/devkit.fish
```

```nu
devkit completions --all nushell | save -f ($nu.default-config-dir | path join completions devkit.nu)
```

Each script registers itself under its own name, so concatenation works in every shell listed above. In zsh, `source` the file rather than autoloading it from `fpath`. Autoloading honors only the first `#compdef` line, while the `compdef` call each script ends with registers all of them.

Which names `--all` covers is read off the command tree, not a list, so it follows whatever has a `completions` subcommand. `devkit-mcp` is absent because `devkit mcp` takes no subcommands.

## PowerShell

`--all` hoists the `using namespace` lines to the top of the combined file. PowerShell rejects a `using` statement that follows any other statement, and the generator repeats the same two at the head of every script, so a plain concatenation would fail to parse from the second script on.

Save the file as UTF-8 or ASCII. Windows PowerShell 5.1 reads a BOM-less UTF-8 `.ps1` as cp1252, so a non-ASCII character in the script would decode to something PowerShell mis-parses. Help text is kept ASCII for that reason, and a test enforces it.
