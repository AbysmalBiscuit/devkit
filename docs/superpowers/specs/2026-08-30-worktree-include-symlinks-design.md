# Preserving symlinks in `worktree_include`

## Problem

`defaults.worktree_include` copies machine-local files from the primary
checkout into each worktree. It dereferences symlinks: a symlinked directory
inside an include is walked, and the files behind it are written into the
destination as real files in a real directory. A symlinked file is written as a
real file holding the target's contents.

`plan_includes` decides what is a directory with `Path::is_dir`, which follows
links, so a symlink to a directory takes the `plan_dir` recursion branch.
`copy_file` copies leaves with `std::fs::copy`, which reads through a link.
Nothing in the walk or the copy ever inspects `symlink_metadata`.

Measured against the code on `main`, with a source tree holding
`inc/link_to_dir -> real_dir` and `inc/link_to_file.txt -> real_file.txt`:

```
planned missing:
  inc\link_to_dir\inner.txt
  inc\link_to_file.txt
copied 2, warnings []

dest/inc/link_to_dir              dir
dest/inc/link_to_dir/inner.txt    file (12 bytes)
dest/inc/link_to_file.txt         file (13 bytes)
```

Nothing in the destination is a link.

Windows junctions behave the same way. Rust reports a junction as a symlink
through `symlink_metadata`, and `Path::is_dir` follows it, so the recursion
branch expands it exactly as it expands a symlink:

```
source junction: is_symlink=true read_link=Ok("...\real_dir")  Path::is_dir=true
plan_includes missing: ["inc\junction\inner.txt"]
dest/inc/junction: is_symlink=false is_dir=true
```

This is wrong for four reasons.

A link is the author's statement that they did not want a second copy. Devkit
overrides that statement without reporting it.

The copies are static. The link target keeps changing and the worktree's copy
does not, so a worktree silently holds stale content that looks current.

Every `issue setup` and `issue checkout-pr` pays the duplication again, so the
cost scales with the number of live worktrees rather than being paid once.

`--dry-run` reports the expansion as the files it will copy, so the preview is
an accurate description of the wrong behaviour, and reviewing it does not
reveal the problem.

The behaviour is not an oversight. `docs/configuration.md` states it, in the
`preserve` section, and ties the two directions together:

> Symlinks are followed, so a link inside the worktree is archived as its
> target's content, matching what `defaults.worktree_include` does in the
> inbound direction.

Someone chose this and wrote it down. The change proposed here contradicts a
documented guarantee, which is why it is a behaviour change requiring a
decision rather than a defect being repaired quietly. It also makes that
sentence false in both halves, so the sentence has to change with the code, and
the question of whether `copy_out` should follow becomes explicit rather than
optional.

The case for changing it anyway: the guarantee describes a mechanism, not an
intent. Nothing in the docs argues that duplicating a link's contents is
what a user wants from an include list; it records what the code does. The
costs above are real whether or not they were anticipated.

## Goal

An include that matches a symlink reproduces the link at the destination,
pointing at the same target, copying no bytes.

## Non-goals

Following the link and copying its contents. That is the behaviour being
removed.

Rewriting link targets. A target is reproduced exactly as the source holds it.

Creating junctions. The standard library cannot create one. A junction source
is reproduced as a symlink. See Limitations.

Symlinks anywhere other than `worktree_include`. `copy_out`, the `preserve`
paths, and the docs cache are out of scope.

## Behaviour

### Detection

A match is a link when `std::fs::symlink_metadata(path)` reports
`file_type().is_symlink()`. This is checked before any `is_dir` test, so a link
never reaches the directory-recursion branch and never reaches `copy_file`.

The check applies to a pattern's direct match and to every entry the recursion
reaches, so a link nested inside an included directory is preserved the same
way as a link named by a pattern.

The walk does not descend through a link. A symlinked directory contributes one
plan entry, the link itself, and none of its contents.

### The target

`std::fs::read_link` returns the target as the link holds it, neither resolved
nor canonicalised. That value is written verbatim.

A relative target therefore resolves inside the worktree, which has the same
shape as the primary checkout, and points at the worktree's own copy. An
absolute target keeps pointing where the author pointed it, usually back at the
primary checkout. Reproducing the author's own text is the only rule that gives
both cases the meaning the author wrote. Rewriting an absolute target to point
into the worktree would be guessing at intent.

A link whose target does not exist is reproduced as a link whose target does
not exist. A broken link in the source is not an error to report; it is a fact
to mirror.

### Creating the link

Unix has one call. Windows has two, and the choice is not inferable from the
target string, so it comes from what the source link resolves to:
`source.is_dir()` follows the link and answers. A source link that does not
resolve takes the file form, which reproduces a broken link.

This lives behind one function in `devkit_common::sys`, which AGENTS.md names
as the platform boundary.

### Failure

Creating a symlink on Windows requires Developer Mode or administrator rights.
Where it is refused, the failure becomes a warning naming the path and the
reason, and the run continues. This matches the existing convention: every copy
error in `apply_includes` is collected as a warning string rather than
propagated, and a failed include has never aborted a worktree setup.

A run that cannot create links is therefore a worktree missing those links,
reported, rather than a worktree holding surprise duplicates or a setup that
died.

### Existing destinations

A link is planned as `missing` or `existing` on the same rule as a file:
whether anything at all is present at the destination path, tested with
`symlink_metadata` so that an existing link counts as present rather than being
followed.

Without `--overwrite`, an existing destination is left alone and reported the
way an existing file is. With `--overwrite`, the destination is removed and the
link is created. Removal has to distinguish a link to a directory from a real
directory: `remove_dir_all` on a symlinked directory would delete the target's
contents, so a link destination is removed with the link-aware call for its
kind, never with a recursive delete.

Re-running without `--overwrite` when the link is already correct copies
nothing and reports nothing new.

### Reporting

A link is not a copied file and is not counted as one. `apply_includes` returns
its count of copied files unchanged in meaning; links are reported separately
so `copied 2 file(s)` never silently includes links.

`--dry-run` names a link as a link, with its target, rather than as a file it
would copy.

## Platform behaviour, measured

An unelevated process on Windows 11 with Developer Mode enabled:

| case | created | `is_symlink` | resolves | `read_link` |
|---|---|---|---|---|
| absolute dir target | yes | true | yes | absolute path |
| absolute file target | yes | true | yes | absolute path |
| relative dir target | yes | true | yes | `..\real_dir` |
| relative file target | yes | true | yes | `..\real_file.txt` |
| broken target | yes | true | no | as written |

Reading through the reproduced relative links returns the target's contents.

Two facts constrain the tests. `powershell` (5.1) refuses to create a symlink
with `NewItemSymbolicLinkElevationRequired` where `pwsh` succeeds, so the
capability is not uniform even on one machine. CI runs the suite on ubuntu,
macos and windows, and a Windows runner may refuse symlink creation entirely.

## Limitations

A junction source becomes a symlink at the destination. They resolve alike for
a reader, but a junction can be created without Developer Mode and a symlink
cannot, so a project that chose junctions deliberately to avoid that
requirement gets a destination it may not be able to create. This affects
projects whose setup scripts create junctions for exactly that reason.

The standard library cannot distinguish a junction from a symlink, and cannot
create a junction, so neither detecting this case nor handling it specially is
possible without Windows API calls beyond `std`. The behaviour is documented
rather than worked around, and the warning on failure is what tells a user it
happened.

## Testing

Every test asserts on what the destination holds, read back with
`symlink_metadata`, not on plan counts. A count passes when the walk finds a
link and the copy then drops it.

The cases: a symlinked file is reproduced as a link, not a copied file; a
symlinked directory is reproduced as a link and its contents are not walked; a
relative target is reproduced verbatim and resolves inside the destination; an
absolute target is reproduced verbatim; a broken link is reproduced broken; an
existing destination is left alone without `--overwrite` and replaced with it;
replacing a link to a directory does not delete the target's contents.

Fixtures come from `tempfile::tempdir()`, per the convention in AGENTS.md.

Each test tries to create its fixture link first and skips when creation is
refused, so a Windows runner without Developer Mode reports a skip rather than
a failure. The skip is explicit and prints why, so a suite that silently stops
covering this on one platform is visible.

## Documentation

Three places, one of which currently states the opposite.

`docs/configuration.md`, the `preserve` section, says symlinks are followed and
archived as their target's content, "matching what `defaults.worktree_include`
does in the inbound direction". After this change the two directions no longer
match. The sentence is rewritten to describe each direction on its own, and
what it says about the inbound one is inverted.

`docs/configuration.md`'s `worktree_include` table row describes recursive
copying and existing-destination rules without mentioning links. It gains the
rule.

`docs/commands.md`'s `sync-includes` entry likewise. Both gain: a matched
symlink is reproduced as a symlink with the same target, its contents are not
copied, and creating one on Windows needs Developer Mode or the link is skipped
with a warning.

## Coordination

`worktree-include-progress` is unmerged and restructures `IncludePlan` from
`{ missing, existing, warnings }` into a per-pattern list with `missing()` and
`existing()` flattening iterators, and adds `plan_includes_with` /
`apply_includes_with` progress variants.

This work adds a third kind of plan entry, so it changes the same type. It
rebases onto that branch rather than racing it, and the entry kind is expressed
in whatever shape that branch lands.

`parallel-includes` adds rayon to the copy and is independent: it parallelises
copying files and does not care that some entries are links.

## Open questions

1. A junction source becomes a symlink that a machine without Developer Mode
   cannot create, which is the exact situation your team's `setup-project.ps1`
   avoids by using junctions. Is a warning enough, or should a Windows failure
   fall back to today's copy-the-contents behaviour rather than leaving the
   worktree without the link?

2. Should the link count appear in `sync-includes` output as its own line
   (`linked 3`) or be folded into the existing per-worktree summary?

3. `copy_out`, used by `issue end --preserve`, dereferences the same way, and
   `docs/configuration.md` documents the two directions as matching. Changing
   only the inbound one breaks that pairing deliberately. Is that right?

   There is a real argument that it is. An include copies into a live worktree
   that still has the primary checkout beside it, so a link keeps working.
   `preserve` archives out of a worktree that is about to be deleted, into a
   location that may outlive the link's target entirely, so a link there could
   be archiving a path that stops resolving the moment the worktree goes. If
   that reasoning holds, the directions should differ and the doc should say
   why. If it does not, `copy_out` wants the same change and this spec should
   grow to cover it.
