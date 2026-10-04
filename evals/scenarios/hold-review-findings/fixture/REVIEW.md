# Review of src/api.py

## F1: page_slice skips the first page

Pages count from 1, but `_tmp` starts page 1 at `1 * size`, so the first page is never returned.
Recommendation: fix it, starting page `n` at `(n - 1) * size`.

## F2: `_tmp` says nothing about what it computes

Recommendation: fix it, renaming `_tmp` to `_page_bounds`.

## F3: get_user_v1 is dead weight

`get_user_v1` is a deprecated alias of `get_user`. Removing it is a breaking change to the public API: callers of the v1 API outside this repository still import it.
Recommendation: remove it.
