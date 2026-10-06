#!/bin/sh
# Install devkit's nightly build, the binaries `main` last built, from the
# rolling `nightly` prerelease into the directory the release installer uses.
set -eu

base="https://github.com/AbysmalBiscuit/devkit/releases/download/nightly"

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *)
        echo "devkit nightly: no build for $(uname -s) $(uname -m)" >&2
        exit 1
        ;;
esac

archive="devkit-${target}.tar.xz"
bin_dir="${CARGO_HOME:-${HOME}/.cargo}/bin"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for file in "$archive" "${archive}.sha256"; do
    curl --proto '=https' --tlsv1.2 -fsSL -o "${tmp}/${file}" "${base}/${file}"
done
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "${archive}.sha256" >/dev/null)
else
    (cd "$tmp" && shasum -a 256 -c "${archive}.sha256" >/dev/null)
fi
tar -xJf "${tmp}/${archive}" -C "$tmp"

mkdir -p "$bin_dir"
for bin in devkit devkitd; do
    install -m 755 "${tmp}/devkit-${target}/${bin}" "${bin_dir}/${bin}"
done

# The plugin's binary bootstrap never replaces binaries it records as
# external, so a plugin update leaves the nightly in place.
state_dir="${XDG_STATE_HOME:-${HOME}/.local/state}/devkit"
mkdir -p "$state_dir"
printf 'external\n' >"${state_dir}/bootstrap-version"

"${bin_dir}/devkit" --version
