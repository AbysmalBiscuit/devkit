# shellcheck shell=bash disable=SC2034
# Sourced by the eval runners. EVAL_HARNESS picks the agent CLI a run drives.
harness=${EVAL_HARNESS:-claude}
case $harness in
  claude) model=${EVAL_MODEL:-claude-opus-5-5} ;;
  codex) model=${EVAL_MODEL:-} ;;
  *) echo "EVAL_HARNESS must be claude or codex, not $harness" >&2; exit 1 ;;
esac
codex_login=${CODEX_HOME:-$HOME/.codex}/auth.json
codex_config=${EVAL_CODEX_CONFIG:+$(realpath "$EVAL_CODEX_CONFIG")}

# Fills a fresh CODEX_HOME with EVAL_CODEX_CONFIG and this machine's Codex
# login, and nothing else of the user's own Codex setup.
codex_home() {
  mkdir -p "$1"
  if [[ -n $codex_config ]]; then
    cp "$codex_config" "$1/config.toml"
  fi
  if [[ -f $codex_login ]]; then
    ln -s "$codex_login" "$1/auth.json"
  fi
}
