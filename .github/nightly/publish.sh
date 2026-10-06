#!/bin/sh
# Replace assets of the `nightly` release with the given files, in order. Each
# takes its live name only after its upload succeeds under a staging name.
set -eu

# A re-run of an older commit's workflow must not overwrite a newer nightly.
if [ "$(gh api "repos/${GH_REPO}/commits/main" --jq .sha)" != "$GITHUB_SHA" ]; then
    echo "::notice::main has moved past ${GITHUB_SHA}; leaving nightly alone"
    exit 0
fi

release=$(gh api "repos/${GH_REPO}/releases/tags/nightly" --jq .id)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# Ids of the release's assets whose name passes the jq condition $1.
asset_ids() {
    gh api --paginate "repos/${GH_REPO}/releases/${release}/assets" --jq ".[] | select($1) | .id"
}

delete_assets() {
    for id in $(asset_ids "$1"); do
        gh api -X DELETE "repos/${GH_REPO}/releases/assets/${id}"
    done
}

for file in "$@"; do
    name=$(basename "$file")
    staged="staged-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}-${name}"
    # Staged copies an interrupted run left behind.
    delete_assets ".name | startswith(\"staged-\") and endswith(\"-${name}\")"
    cp "$file" "${stage}/${staged}"
    gh release upload nightly "${stage}/${staged}"
    delete_assets ".name == \"${name}\""
    gh api -X PATCH "repos/${GH_REPO}/releases/assets/$(asset_ids ".name == \"${staged}\"")" \
        -f name="$name" >/dev/null
done
