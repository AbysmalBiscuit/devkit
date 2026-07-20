#!/usr/bin/env bash
# Behavioural tests for hooks/bootstrap-binaries.
#
# The hook installs binaries from a GitHub release, so every case here runs it
# against a stubbed curl/powershell.exe on a stubbed PATH — nothing is
# downloaded and nothing outside the test's temp dirs is touched.
#
# Run: bash tests/hooks/bootstrap-binaries.test.sh

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOOK="${REPO_ROOT}/hooks/bootstrap-binaries"
MANIFEST="${REPO_ROOT}/.claude-plugin/plugin.json"

VERSION=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST" | head -1)
if [ -z "$VERSION" ]; then
    echo "could not read a version from ${MANIFEST}" >&2
    exit 1
fi

RELEASE="https://github.com/AbysmalBiscuit/devkit/releases/download/v${VERSION}"
EXPECTED_CURL="curl --proto =https --tlsv1.2 -LsSf --connect-timeout 10 --max-time 300 ${RELEASE}/devkit-installer.sh
installer-ran"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

STUB="${WORK}/stub"
BIN="${WORK}/bin"
mkdir -p "$STUB" "$BIN"
CALLS="${WORK}/calls"

cat >"${STUB}/curl" <<'EOF'
#!/usr/bin/env bash
echo "curl $*" >>"$CURL_LOG"
[ "${CURL_FAIL:-0}" = "1" ] && exit 1
# Emit the "installer" the hook pipes into sh.
echo "echo installer-ran >>\"$CURL_LOG\""
EOF

cat >"${STUB}/powershell.exe" <<'EOF'
#!/usr/bin/env bash
echo "powershell $*" >>"$CURL_LOG"
EOF

cat >"${STUB}/uname" <<'EOF'
#!/usr/bin/env bash
echo "${FAKE_UNAME:-Linux}"
EOF

chmod +x "${STUB}/curl" "${STUB}/powershell.exe" "${STUB}/uname"

for b in devkit lockm devkit-mcp; do
    printf '#!/usr/bin/env bash\n' >"${BIN}/${b}"
    chmod +x "${BIN}/${b}"
done

pass=0
fail=0
state=""
state_seq=0
last_exit=0

check() {
    local label="$1" expected="$2" actual="$3"
    if [ "$expected" = "$actual" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL %s\n  expected: %s\n  actual:   %s\n' "$label" "$expected" "$actual" >&2
    fi
}

new_state() {
    state_seq=$((state_seq + 1))
    state="${WORK}/state-${state_seq}"
    mkdir -p "$state"
}

# `binaries` selects whether PATH also carries devkit/lockm/devkit-mcp, which is
# what the hook probes. env -i keeps the caller's real devkit off PATH.
run_hook() {
    local binaries="$1"
    shift
    local path="$STUB"
    [ "$binaries" = "with-binaries" ] && path="${BIN}:${STUB}"
    rm -f "$CALLS"
    env -i HOME="$WORK" PATH="${path}:/usr/bin:/bin" XDG_STATE_HOME="$state" \
        CURL_LOG="$CALLS" "$@" bash "$HOOK" >"${WORK}/out" 2>&1
    last_exit=$?
}

run_wrapper() {
    rm -f "$CALLS"
    env -i HOME="$WORK" PATH="${BIN}:${STUB}:/usr/bin:/bin" XDG_STATE_HOME="$state" \
        CURL_LOG="$CALLS" bash "${REPO_ROOT}/hooks/run-hook.cmd" bootstrap-binaries \
        >"${WORK}/out" 2>&1
    last_exit=$?
}

stamp() { cat "${state}/devkit/bootstrap-version" 2>/dev/null || echo NONE; }
marker() { cat "${state}/devkit/bootstrap-failed" 2>/dev/null || echo NONE; }
calls() { cat "$CALLS" 2>/dev/null || echo NONE; }
set_stamp() { mkdir -p "${state}/devkit" && printf '%s\n' "$1" >"${state}/devkit/bootstrap-version"; }

echo "testing hooks/bootstrap-binaries against plugin version ${VERSION}"

new_state
run_hook with-binaries DEVKIT_NO_BOOTSTRAP=1
check "opt-out exits 0" 0 "$last_exit"
check "opt-out touches nothing" NONE "$(stamp)"

new_state
run_hook without-binaries
check "missing binaries installs" 0 "$last_exit"
check "install records the version" "$VERSION" "$(stamp)"
check "install pins the release" "$EXPECTED_CURL" "$(calls)"

# A pre-existing install the hook did not perform must survive untouched.
new_state
run_hook with-binaries
check "unstamped binaries are external" external "$(stamp)"
check "external install skips the network" NONE "$(calls)"

new_state
set_stamp "$VERSION"
run_hook with-binaries
check "current version is a no-op" NONE "$(calls)"
check "current version keeps the stamp" "$VERSION" "$(stamp)"

# The update path: a plugin update moves plugin.json's version past the stamp.
new_state
set_stamp 0.0.1
run_hook with-binaries
check "stale stamp reinstalls" "$VERSION" "$(stamp)"
check "reinstall pins the new release" "$EXPECTED_CURL" "$(calls)"

new_state
run_hook without-binaries CURL_FAIL=1
check "failed install still exits 0" 0 "$last_exit"
check "failed install records no version" NONE "$(stamp)"
check "failed install marks the version" "$VERSION" "$(marker)"

# Having marked a failure, the hook must not retry it every session.
run_hook without-binaries
check "marked failure suppresses retry" NONE "$(calls)"

new_state
run_hook without-binaries FAKE_UNAME=MINGW64_NT-10.0
check "windows exits 0" 0 "$last_exit"
check "windows invokes the ps1 installer" \
    "powershell -NoProfile -ExecutionPolicy Bypass -Command irm ${RELEASE}/devkit-installer.ps1 | iex" \
    "$(calls)"

# The wrapper hooks.json actually invokes, rather than the hook directly.
new_state
run_wrapper
check "run-hook.cmd dispatches on unix" external "$(stamp)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
