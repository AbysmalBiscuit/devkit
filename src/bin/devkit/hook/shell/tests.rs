//! The shell write stage in-process: a command's analysis, its evaluation and
//! the write gate, against a registry of the test's own. The envelope, the
//! exit code and the config that turns the stage on are pinned through the
//! binary in `tests/harness_shell_writes.rs`.

use devkit_common::harness::HarnessPolicy;
use devkit_config::PolicyAction;

use super::{super::gate::fixture::Project, *};

fn run_with(p: &Project, holder: &str, command: &str, policy: HarnessPolicy) -> WriteVerdict {
    let analysis = devkit_command::analyze(command, &context(Dialect::Bash, Some(p.path())));
    write_stage(
        &analysis,
        &policy,
        false,
        Some(holder),
        &p.gate(),
        &p.checkout(),
        p.path(),
    )
}

/// The deny reason `command` earns, or `None` when it may run.
fn denial(p: &Project, holder: &str, command: &str) -> Option<String> {
    let verdict = run_with(p, holder, command, HarnessPolicy::default());
    (!verdict.blocks.is_empty()).then(|| verdict.blocks.join("\n"))
}

fn warn_on_unresolved() -> HarnessPolicy {
    HarnessPolicy {
        unresolved_writes: PolicyAction::Warn,
        ..HarnessPolicy::default()
    }
}

fn row(path: &str, holder: &str) -> (String, String) {
    (path.to_string(), holder.to_string())
}

fn mkdir(p: &Project, dir: &str) {
    std::fs::create_dir_all(p.path().join(dir)).unwrap();
}

fn touch(p: &Project, file: &str) {
    std::fs::write(p.path().join(file), "x").unwrap();
}

#[test]
fn own_and_ancestor_claims_allow() {
    let p = Project::new();
    p.hold("S1", "a.txt");
    assert_eq!(denial(&p, "S1", "echo x > a.txt"), None);
    assert_eq!(denial(&p, "S1/a1", "echo x > a.txt"), None);
}

#[test]
fn both_ends_of_a_rename_are_checked() {
    let p = Project::new();
    p.hold("S2", "b.txt");
    assert!(denial(&p, "S1", "mv a.txt b.txt").is_some());
}

/// A `..` component, or an absolute later argument, carries a temp-derived
/// path back out of the directory that made it uncontendable.
#[test]
fn a_temp_path_that_leaves_its_fresh_directory_is_not_exempt() {
    let p = Project::new();
    p.hold("B", "victim.txt");
    let outside = p.path().join("victim.txt");
    let outside = outside.to_string_lossy();
    let cases = [
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(dir='.'); \
             open(os.path.join(d, '../victim.txt'), 'w').write('x')\""
            .to_string(),
        format!(
            "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); \
             open(os.path.join(d, '{outside}'), 'w').write('x')\""
        ),
        "python3 -c \"import tempfile, pathlib; d = tempfile.mkdtemp(); \
             pathlib.Path(d, '../victim.txt').write_text('x')\""
            .to_string(),
        "bun -e \"const fs = require('fs'); const path = require('path'); \
             const d = fs.mkdtempSync('./fresh-'); \
             fs.writeFileSync(path.join(d, '../victim.txt'), 'x')\""
            .to_string(),
        "D=$(mktemp -d ./fresh.XXXXXX); echo x > \"$D/../victim.txt\"".to_string(),
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).joinpath('../victim.txt').write_text('x')\""
            .to_string(),
        "python3 -c \"import tempfile, pathlib; \
         (pathlib.Path(tempfile.mkdtemp(dir='.')).parent / 'victim.txt').write_text('x')\""
            .to_string(),
        "python3 -c \"import tempfile, pathlib; \
         p = pathlib.Path(tempfile.mkdtemp()).joinpath('victim.txt'); \
         open(p.name, 'w').write('x')\""
            .to_string(),
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).parent.rename('moved')\""
            .to_string(),
    ];
    for c in &cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("could not be determined"), "{c}: {reason}");
    }
}

/// A destination outside the fresh directory is an ordinary target, so it is
/// claimed and the holder is named rather than reported as undeterminable.
#[test]
fn a_copy_or_move_out_of_a_fresh_directory_names_the_holder() {
    let p = Project::new();
    p.hold("B", "victim.txt");
    let cases = [
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).copy('victim.txt')\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).move('victim.txt')\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).rename('victim.txt')\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("locked by another agent"), "{c}: {reason}");
    }
}

/// An unknown suffix cannot show that the destination stayed inside the fresh
/// directory, so it does not inherit the exemption.
#[test]
fn an_unknown_component_does_not_keep_a_temp_path_exempt() {
    let p = Project::new();
    let cases = [
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); \
         open(os.path.join(d, os.environ['REL']), 'w').write('x')\"",
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(dir=os.environ['X']); \
         open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
    ];
    for c in cases {
        assert!(denial(&p, "S1", c).is_some(), "allowed: {c}");
    }
}

/// A fresh random name cannot be held by anyone, but the directory it is made
/// in can be, and that claim covers everything born under it.
#[test]
fn a_fresh_entry_under_a_held_directory_is_denied() {
    let p = Project::new();
    p.hold("B", ".");
    let cases = [
        "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(dir='.'); f.write(b'x')\"",
        "T=$(mktemp -p .); echo x > \"$T\"",
        "T=$(mktemp -p . fresh.XXXXXX); echo x > \"$T\"",
        "D=$(mktemp -d ./fresh.XXXXXX); echo x > \"$D\"",
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(dir='.'); \
         open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).write_text('x')\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("locked by another agent"), "{reason}");
    }
}

/// A link made inside a fresh directory can point out of it, and every write
/// through the link lands wherever it points, so the fresh subtree stops
/// proving anything.
#[test]
fn a_link_made_in_a_fresh_directory_loses_the_exemption() {
    let p = Project::new();
    mkdir(&p, "src");
    touch(&p, "src/model.rs");
    p.hold("B", "src/model.rs");
    let cases = [
        "python3 -c \"import os, tempfile; d = tempfile.mkdtemp(dir='.'); \
         os.symlink('../src', os.path.join(d, 'link')); \
         open(os.path.join(d, 'link/model.rs'), 'w').write('x')\"",
        "python3 -c \"import pathlib, tempfile; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).joinpath('link').symlink_to('../src')\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('./fresh-'); \
         fs.symlinkSync('../src', path.join(d, 'link')); \
         fs.writeFileSync(path.join(d, 'link/model.rs'), 'x')\"",
        // A hard link names the source's inode, so a write through it lands on
        // the source wherever that is.
        "python3 -c \"import os, tempfile; d = tempfile.mkdtemp(dir='.'); \
         os.link('src/model.rs', os.path.join(d, 'link')); \
         open(os.path.join(d, 'link'), 'w').write('x')\"",
        "python3 -c \"import pathlib, tempfile; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).joinpath('link').hardlink_to('src/model.rs')\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('./fresh-'); \
         fs.linkSync('src/model.rs', path.join(d, 'link')); \
         fs.writeFileSync(path.join(d, 'link'), 'x')\"",
        // Each target stays inside the directory by name, but the second is
        // read through the first, which leads out of it.
        "python3 -c \"import os, tempfile; d = tempfile.mkdtemp(dir='.'); \
         os.symlink('.', os.path.join(d, 'a')); \
         os.symlink('a/../src', os.path.join(d, 'b')); \
         open(os.path.join(d, 'b/model.rs'), 'w').write('x')\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('./fresh-'); \
         fs.symlinkSync('.', path.join(d, 'a')); \
         fs.symlinkSync('a/../src', path.join(d, 'b')); \
         fs.writeFileSync(path.join(d, 'b/model.rs'), 'x')\"",
        // No claim is needed for the rule to hold: the link itself is what the
        // analyzer cannot follow.
        "python3 -c \"import os, tempfile; d = tempfile.mkdtemp(); \
         os.symlink('real.txt', os.path.join(d, 'link'))\"",
        "T=$(mktemp); ln -sf src/model.rs \"$T\"; echo x > \"$T\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("point outside it"), "{reason}");
    }
}

/// A link moved, copied, or unpacked into a fresh directory aliases a path
/// outside it just as one created there does, and the analyzer cannot see
/// whether what it placed is a link. Once anything is placed, the other writes
/// under fresh paths in the command stop being exempt.
#[test]
fn a_placement_in_a_fresh_directory_loses_the_exemption() {
    let p = Project::new();
    mkdir(&p, "src");
    touch(&p, "src/model.rs");
    p.hold("B", "src/model.rs");
    let cases = [
        "python3 -c \"import os, tempfile; d = tempfile.mkdtemp(); \
         os.rename('link', os.path.join(d, 'link')); \
         open(os.path.join(d, 'link', 'model.rs'), 'w').write('x')\"",
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         shutil.move('link', d); \
         open(os.path.join(d, 'link', 'model.rs'), 'w').write('x')\"",
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         shutil.copy2('link', d, follow_symlinks=False); \
         open(os.path.join(d, 'link', 'model.rs'), 'w').write('x')\"",
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         shutil.copytree('links', os.path.join(d, 't'), symlinks=True); \
         open(os.path.join(d, 't', 'link', 'model.rs'), 'w').write('x')\"",
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         shutil.unpack_archive('links.tar', d); \
         open(os.path.join(d, 'link', 'model.rs'), 'w').write('x')\"",
        // A second placement can land through the first.
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         shutil.copy2('link', d, follow_symlinks=False); \
         shutil.copy('model.rs', os.path.join(d, 'link'))\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('./fresh-'); \
         fs.cpSync('link', path.join(d, 'link')); \
         fs.writeFileSync(path.join(d, 'link/model.rs'), 'x')\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('./fresh-'); \
         fs.renameSync('link', path.join(d, 'link')); \
         fs.writeFileSync(path.join(d, 'link/model.rs'), 'x')\"",
        "D=$(mktemp -d); mv link \"$D\"; tar -xf links.tar -C \"$D\"",
        // These copies keep a link a link.
        "D=$(mktemp -d); cp -P link \"$D\"; tar -xf links.tar -C \"$D\"",
        "D=$(mktemp -d); cp -a link \"$D\"; tar -xf links.tar -C \"$D\"",
        // A removal below a placed link deletes through it.
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         shutil.copy2('link', d, follow_symlinks=False); \
         shutil.rmtree(os.path.join(d, 'link', 'sub'))\"",
    ];
    let allowed: Vec<_> = cases
        .into_iter()
        .filter(|c| {
            !denial(&p, "S1", c).is_some_and(|r| r.contains("can be a link leading out of it"))
        })
        .collect();
    assert!(allowed.is_empty(), "allowed: {allowed:#?}");
}

/// Moving or plainly copying a file into a temp directory, writing a temp file
/// and moving it out, and removing the temp directory itself place nothing a
/// later write could pass through.
#[test]
fn an_ordinary_temp_move_keeps_the_exemption() {
    let p = Project::new();
    let cases = [
        "python3 -c \"import shutil, tempfile; shutil.move('data.csv', tempfile.mkdtemp())\"",
        "D=$(mktemp -d); cp data.csv \"$D\"",
        "T=$(mktemp); echo x > \"$T\"; mv \"$T\" out.txt",
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         p = os.path.join(d, 'out'); open(p, 'w').write('x'); shutil.move(p, 'out.txt')\"",
        "D=$(mktemp -d); echo x > \"$D\"; rm -rf \"$D\"",
        // Removing the fresh directory itself unlinks what was placed in it
        // and follows none of it.
        "D=$(mktemp -d); cp -r src \"$D\"; rm -rf \"$D\"",
        // A rename inside the fresh directory brings nothing in from outside.
        "python3 -c \"import os, tempfile; d = tempfile.mkdtemp(); \
         p = os.path.join(d, 'x.tmp'); open(p, 'w').write('x'); \
         os.replace(p, os.path.join(d, 'x'))\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('./fresh-'); \
         fs.writeFileSync(path.join(d, 'a.tmp'), 'x'); \
         fs.renameSync(path.join(d, 'a.tmp'), path.join(d, 'a'))\"",
        // A plain copy writes the source's content, never a link.
        "T=$(mktemp); cp cfg.toml \"$T\"; sed -i 's/a/b/' \"$T\"; mv \"$T\" cfg.toml",
        "python3 -c \"import os, shutil, tempfile; d = tempfile.mkdtemp(); \
         p = os.path.join(d, 'cfg'); shutil.copy('cfg', p); open(p, 'a').write('x')\"",
    ];
    let denied: Vec<_> = cases
        .into_iter()
        .filter_map(|c| denial(&p, "S1", c).map(|r| (c, r)))
        .collect();
    assert!(denied.is_empty(), "denied: {denied:#?}");
}

/// `move` and `move_into` rename the source away, so the source is written
/// too, not just the destination.
#[test]
fn a_move_names_the_source_it_renames_away() {
    let p = Project::new();
    touch(&p, "held.txt");
    mkdir(&p, "free");
    p.hold("B", "held.txt");
    let cases = [
        "python3 -c \"import pathlib; pathlib.Path('held.txt').move('free.txt')\"",
        "python3 -c \"import pathlib; pathlib.Path('held.txt').move_into('free')\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("held.txt (held by B)"), "{reason}");
    }
}

/// `mkdir` and `rmdir` on a resolved `pathlib.Path` write, and reach the
/// registry whatever the receiver was made from.
#[test]
fn a_pathlib_directory_method_reaches_the_registry() {
    let p = Project::new();
    mkdir(&p, "build");
    p.hold("B", "build");
    let cases = [
        "python3 -c \"import pathlib; pathlib.Path('build').rmdir()\"",
        "python3 -c \"import pathlib; pathlib.Path('build/sub').mkdir()\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("locked by another agent"), "{reason}");
    }
}

/// A write mode still writes when the path it opens could not be determined,
/// and a read mode still writes nothing.
#[test]
fn an_open_on_an_undetermined_path_reports_the_write() {
    let p = Project::new();
    let write = "python3 -c \"import tempfile, pathlib; \
                 p = pathlib.Path(tempfile.mkdtemp(dir='.')).parent.joinpath('victim.txt'); \
                 p.open('w').write('x')\"";
    let reason = denial(&p, "S1", write).unwrap_or_else(|| panic!("allowed"));
    assert!(reason.contains("could not be determined"), "{reason}");

    let read = "python3 -c \"import tempfile, pathlib; \
                p = pathlib.Path(tempfile.mkdtemp(dir='.')).parent.joinpath('notes.txt'); \
                p.open().read()\"";
    assert_eq!(denial(&p, "S1", read), None);
}

/// A receiver whose filesystem behaviour could not be established may still be
/// a path, so `open` on it reads its mode like any other.
#[test]
fn an_open_on_an_undetermined_receiver_reports_the_write() {
    let p = Project::new();
    let cases = [
        "python3 -c \"import pathlib; \
         pathlib.Path('safe.txt').with_stem('victim').open('w').write('x')\"",
        "python3 -c \"import pathlib, tempfile; \
         pathlib.Path(tempfile.mkdtemp(dir='.')).joinpath('out.txt', 'child') \
         .parent.open('w').write('x')\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("could not be determined"), "{reason}");
    }

    let read = "python3 -c \"import pathlib; \
                pathlib.Path('safe.txt').with_stem('notes').open().read()\"";
    assert_eq!(denial(&p, "S1", read), None);
}

/// A quoted `mktemp` argument names the same directory the shell would use.
#[test]
fn a_quoted_mktemp_directory_is_read_without_its_quotes() {
    let p = Project::new();
    mkdir(&p, "held");
    p.hold("B", "held");
    let cases = [
        "T=$(mktemp -p 'held' fresh.XXXXXX); echo x > \"$T\"",
        "T=$(mktemp -p \"held\"); echo x > \"$T\"",
    ];
    for c in cases {
        let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
        assert!(reason.contains("held (held by B)"), "{reason}");
    }
}

/// A claim below a directory reaches no name created fresh in it: the fresh
/// name is a sibling of the held path, never a parent of it.
#[test]
fn a_claim_below_a_directory_leaves_a_fresh_entry_in_it_alone() {
    let p = Project::new();
    mkdir(&p, "src");
    touch(&p, "src/model.rs");
    p.hold("B", "src/model.rs");
    let cases = [
        "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(dir='.'); f.write(b'x')\"",
        "T=$(mktemp -p .); echo x > \"$T\"",
        // Everything under a directory created a moment ago is itself fresh, so
        // a recursive writer rooted there reaches nothing anyone holds.
        "rm -rf \"$(mktemp -d -p .)\"",
    ];
    for c in cases {
        assert_eq!(denial(&p, "S1", c), None, "denied: {c}");
    }
    assert_eq!(p.rows(), [row("src/model.rs", "B")]);
}

/// A claim above a directory covers every name created in it, and the refusal
/// names that claim rather than the directory the command asked about.
#[test]
fn a_claim_above_a_directory_blocks_a_fresh_entry_and_names_the_claim() {
    let p = Project::new();
    mkdir(&p, "src");
    p.hold("B", ".");
    let c = "T=$(mktemp -p src); echo x > \"$T\"";
    let reason = denial(&p, "S1", c).unwrap_or_else(|| panic!("allowed: {c}"));
    assert!(reason.contains(". (held by B)"), "{reason}");
}

/// A writer that rewrites files it did not create still conflicts with a claim
/// under its directory, which is what the broader check is for.
#[test]
fn a_tree_writer_still_conflicts_with_a_claim_under_its_directory() {
    let p = Project::new();
    mkdir(&p, "build");
    touch(&p, "build/out.o");
    p.hold("B", "build/out.o");
    let reason = denial(&p, "S1", "rm -rf build").unwrap_or_else(|| panic!("allowed"));
    assert!(reason.contains("locked by another agent"), "{reason}");
}

/// `mktemp` failure leaves the variable empty, which turns the rest of the
/// word into an absolute path nobody claimed on this session's behalf.
#[test]
fn a_failed_mktemp_substitution_does_not_launder_the_suffix() {
    let p = Project::new();
    p.hold("B", "victim.txt");
    let victim = p.path().join("victim.txt");
    let c = format!(
        "D=$(mktemp -d /missing-parent/XXXXXX); echo x > \"$D{}\"",
        victim.to_string_lossy()
    );
    assert!(denial(&p, "S1", &c).is_some(), "allowed: {c}");
}

/// The exemption a fresh directory exists for: a destination that provably
/// stays inside a directory created fresh under a random name.
#[test]
fn a_write_that_stays_inside_a_fresh_directory_is_allowed() {
    let p = Project::new();
    p.hold("B", "victim.txt");
    let cases = [
        "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); \
         open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
        "python3 -c \"import tempfile, pathlib; d = tempfile.mkdtemp(); \
         pathlib.Path(d, 'sub', 'out.txt').write_text('x')\"",
        "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(); f.write(b'x')\"",
        "bun -e \"const fs = require('fs'); const path = require('path'); \
         const d = fs.mkdtempSync('/tmp/fresh-'); \
         fs.writeFileSync(path.join(d, 'out.txt'), 'x')\"",
        "T=$(mktemp); echo x > \"$T\"",
        "python3 -c \"import tempfile, pathlib; \
         pathlib.Path(tempfile.mkdtemp()).joinpath('sub', 'out.txt').write_text('x')\"",
    ];
    for c in cases {
        assert!(denial(&p, "S1", c).is_none(), "denied: {c}");
    }
    assert_eq!(p.rows(), [row("victim.txt", "B")]);
}

#[test]
fn a_root_claim_does_not_unblock_an_unresolved_write() {
    let p = Project::new();
    p.hold("S1", ".");
    let reason = denial(&p, "S1", "echo x > \"$OUT\"").expect("denied");
    assert!(reason.contains("could not be determined"), "{reason}");
}

#[test]
fn a_warning_cannot_override_a_known_conflict() {
    let p = Project::new();
    p.hold("S2", "a.txt");
    let verdict = run_with(
        &p,
        "S1",
        "echo x > a.txt; echo y > \"$OUT\"",
        warn_on_unresolved(),
    );
    assert!(!verdict.blocks.is_empty(), "{verdict:?}");
}

#[test]
fn an_argv_bound_python_edit_claims_its_target() {
    let p = Project::new();
    let command = "f=src/a.ts; python3 - \"$f\" <<'PY'\nimport sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text('x')\nPY\n";
    assert_eq!(denial(&p, "S1", command), None);
    assert_eq!(p.rows(), [row("src/a.ts", "S1")]);
}

#[test]
fn read_only_commands_and_quoted_text_claim_nothing() {
    let p = Project::new();
    for command in [
        "cat README.md | rg foo",
        "git commit -m \"echo x > y.txt\"",
        "node -e \"console.log(require.resolve('x'))\"",
    ] {
        assert_eq!(denial(&p, "S1", command), None, "{command}");
    }
    assert!(p.rows().is_empty(), "{:?}", p.rows());
}

#[test]
fn a_tree_writer_is_checked_and_claims_nothing() {
    let p = Project::new();
    assert_eq!(denial(&p, "S1", "cargo fmt"), None);
    assert!(p.rows().is_empty());
    p.hold("S2", "src/lib.rs");
    assert!(denial(&p, "S1", "cargo fmt").is_some());
    assert_eq!(p.rows(), [row("src/lib.rs", "S2")]);
}

/// A mode or owner change leaves the contents alone, so it claims nothing, but
/// it still changes a file another session may hold.
#[test]
fn a_permission_change_is_checked_and_claims_nothing() {
    let commands = [
        "chmod +x run.sh",
        "chmod -x run.sh",
        "chown me:staff run.sh",
        "python3 -c \"import os; os.chmod('run.sh', 0o755)\"",
        "python3 -c \"import pathlib; pathlib.Path('run.sh').chmod(0o755)\"",
        "node -e \"require('fs').chmodSync('run.sh', 0o755)\"",
        "bun -e \"import { chown } from 'node:fs/promises'; await chown('run.sh', 1, 1)\"",
    ];
    let p = Project::new();
    for command in commands {
        assert_eq!(denial(&p, "S1", command), None, "{command}");
    }
    assert!(p.rows().is_empty(), "{:?}", p.rows());
    for held in ["run.sh", "."] {
        let p = Project::new();
        p.hold("S2", held);
        for command in commands {
            let reason =
                denial(&p, "S1", command).unwrap_or_else(|| panic!("{held}: allowed: {command}"));
            assert!(reason.contains("S2"), "{held}: {command}: {reason}");
        }
        assert_eq!(p.rows(), [row(held, "S2")]);
    }
}

/// Only a recursive or globbed change reaches the paths under a directory.
#[test]
fn a_permission_change_reaches_under_a_directory_only_when_recursive() {
    let p = Project::new();
    mkdir(&p, "bin");
    p.hold("S2", "bin/tool");
    assert_eq!(denial(&p, "S1", "chmod 755 bin"), None);
    for command in ["chmod -R 755 bin", "chown -R me bin", "chmod +x bin/*"] {
        let reason = denial(&p, "S1", command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("S2"), "{command}: {reason}");
    }
    assert_eq!(p.rows(), [row("bin/tool", "S2")]);
}

#[test]
fn a_permission_change_on_an_undetermined_path_is_unresolved() {
    let p = Project::new();
    let reason = denial(&p, "S1", "chmod +x \"$F\"").expect("denied");
    assert!(reason.contains("could not be determined"), "{reason}");
}

/// Every path a glob or a `find` can hand the command lies under one
/// directory, so that directory is claimed, and the claim reserves it: a
/// later write under it by another session is refused.
#[test]
fn a_bounded_file_set_claims_its_directory() {
    for command in [
        "sed -i 's/a/b/' src/*.rs",
        "rm -f src/gen/*.rs src/*.rs",
        "for f in src/*.rs; do sed -i 's/a/b/' \"$f\"; done",
        "find src -name '*.rs' -exec sed -i 's/a/b/' {} +",
        "d=src; rm -f \"$d\"/*.orig",
    ] {
        let p = Project::new();
        assert_eq!(denial(&p, "S1", command), None, "{command}");
        assert_eq!(p.rows(), [row("src", "S1")], "{command}");
        let reason = denial(&p, "S2", "echo x > src/new.rs").expect(command);
        assert!(reason.contains("S1"), "{command}: {reason}");
    }
}

#[test]
fn a_bounded_file_set_is_denied_by_a_claim_on_either_side_of_its_directory() {
    for held in ["src/a.rs", "src", "."] {
        let p = Project::new();
        p.hold("S2", held);
        let reason = denial(&p, "S1", "sed -i 's/a/b/' src/*.rs").expect(held);
        assert!(reason.contains("S2"), "{held}: {reason}");
        assert_eq!(p.rows(), [row(held, "S2")]);
    }
}

/// A session may write through its own claim or an ancestor's, never through
/// a sub-agent's.
#[test]
fn a_bounded_file_set_follows_write_ownership() {
    let p = Project::new();
    assert_eq!(denial(&p, "S1/a1", "echo x > src/a.rs"), None);
    assert!(denial(&p, "S1", "sed -i 's/a/b/' src/*.rs").is_some());

    let p = Project::new();
    p.hold("S1", "src");
    assert_eq!(denial(&p, "S1/a1", "sed -i 's/a/b/' src/*.rs"), None);
    assert_eq!(p.rows(), [row("src", "S1")]);
}

/// A session's own claim on one file under the directory does not reserve
/// the rest of it, so the directory is claimed as well.
#[test]
fn an_own_claim_below_the_directory_does_not_cover_it() {
    let p = Project::new();
    p.hold("S1", "src/a.rs");
    assert_eq!(denial(&p, "S1", "sed -i 's/a/b/' src/*.rs"), None);
    assert_eq!(p.rows(), [row("src", "S1"), row("src/a.rs", "S1")]);
    assert!(denial(&p, "S2", "echo x > src/b.rs").is_some());
}

/// A bound that is the whole checkout, or that no checkout contains, would
/// need a claim nobody should hold for a session, so the write stays under
/// `unresolved_writes`.
#[test]
fn a_bound_that_is_not_a_directory_below_a_checkout_root_is_unresolved() {
    let outside = tempfile::tempdir().unwrap();
    let outside = outside.path().to_string_lossy().into_owned();
    for command in [
        "rm -f *.log".to_string(),
        "find . -name '*.orig' -exec rm {} +".to_string(),
        format!("rm -f '{outside}'/sub/*.log"),
    ] {
        let p = Project::new();
        let reason = denial(&p, "S1", &command).expect(&command);
        assert!(reason.contains("literal path"), "{command}: {reason}");
        assert!(p.rows().is_empty(), "{command}: {:?}", p.rows());

        let p = Project::new();
        let verdict = run_with(&p, "S1", &command, warn_on_unresolved());
        assert!(verdict.blocks.is_empty(), "{command}: {verdict:?}");
        assert!(!verdict.warnings.is_empty(), "{command}: {verdict:?}");
        assert!(p.rows().is_empty(), "{command}: {:?}", p.rows());
    }
}

/// Shapes whose matches can leave the directory the pattern names stay
/// unresolved.
#[test]
fn a_file_set_that_can_leave_its_directory_is_unresolved() {
    let p = Project::new();
    for command in [
        "rm -f src/*/../../victim.txt",
        "rm -f src/.*",
        "for f in src/*.rs; do rm -f $f; done",
        "for f in src/*.rs; do rm -f \"$f.bak\"; done",
        "find -L src -exec rm {} +",
        "find src lib -exec rm {} +",
    ] {
        let reason = denial(&p, "S1", command).expect(command);
        assert!(
            reason.contains("could not be determined"),
            "{command}: {reason}"
        );
    }
    assert!(p.rows().is_empty(), "{:?}", p.rows());
}

#[test]
fn unsupported_language_and_script_file_policies() {
    let p = Project::new();
    assert!(denial(&p, "S1", "perl -e 'print 1'").is_some());
    assert_eq!(denial(&p, "S1", "python3 tools/gen.py"), None);

    let open = HarnessPolicy {
        unsupported_language: PolicyAction::Allow,
        script_files: PolicyAction::Block,
        ..HarnessPolicy::default()
    };
    assert!(
        run_with(&p, "S1", "perl -e 'print 1'", open)
            .blocks
            .is_empty()
    );
    assert!(
        !run_with(&p, "S1", "python3 tools/gen.py", open)
            .blocks
            .is_empty()
    );
}

/// A command something else already blocked never reaches the registry, so
/// it claims nothing on the way to its denial.
#[test]
fn a_blocked_command_claims_nothing() {
    let p = Project::new();
    let analysis =
        devkit_command::analyze("echo x > a.txt", &context(Dialect::Bash, Some(p.path())));
    let verdict = write_stage(
        &analysis,
        &HarnessPolicy::default(),
        true,
        Some("S1"),
        &p.gate(),
        &p.checkout(),
        p.path(),
    );
    assert!(verdict.blocks.is_empty(), "{verdict:?}");
    assert!(p.rows().is_empty(), "{:?}", p.rows());
}

#[test]
fn a_claim_without_a_holder_is_denied() {
    let p = Project::new();
    let analysis =
        devkit_command::analyze("echo x > a.txt", &context(Dialect::Bash, Some(p.path())));
    let verdict = write_stage(
        &analysis,
        &HarnessPolicy::default(),
        false,
        None,
        &p.gate(),
        &p.checkout(),
        p.path(),
    );
    assert!(verdict.blocks[0].contains("session_id"), "{verdict:?}");
    assert!(p.rows().is_empty(), "{:?}", p.rows());
}
