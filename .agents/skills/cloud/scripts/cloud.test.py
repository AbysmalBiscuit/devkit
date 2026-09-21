import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"


def merge_tree_takes_trees():
    """Whether this git's merge-tree accepts trees where it wants commits.

    git-commit-patch.py merges the selected and staged trees directly. Git
    rejected tree arguments until it learned to take them, so on an older git
    the patch commit cannot run at all and its test has nothing to assert.
    """
    probe = subprocess.run(
        ["git", "-C", str(ROOT), "merge-tree", "--write-tree",
         f"--merge-base={EMPTY_TREE}", EMPTY_TREE, EMPTY_TREE],
        capture_output=True,
    )
    return probe.returncode == 0


class CloudHooks(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="cloud hooks ")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        shutil.copytree(ROOT / ".agents", self.root / ".agents", ignore=shutil.ignore_patterns("__pycache__"))
        self.env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        self.env.pop("CLAUDE_CODE_REMOTE", None)
        self.env.pop("DEVKIT_CLOUD", None)
        self.env.pop("CLOUD_AGENT", None)
        self.env.pop("CLOUD_AGENT_TYPE", None)
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Cloud Test", GIT_AUTHOR_EMAIL="cloud@example.invalid",
                        GIT_COMMITTER_NAME="Cloud Test", GIT_COMMITTER_EMAIL="cloud@example.invalid")
        subprocess.run(
            ["git", "-C", str(self.root), "init", "-b", "test-cloud"],
            env=self.env, check=True, capture_output=True,
        )

    def run_script(self, name, *args, cloud=False, **overrides):
        env = self.env.copy()
        if cloud:
            env["CLOUD_AGENT"] = "true"
        for key, value in overrides.items():
            env.pop(key, None) if value is None else env.update({key: value})
        # An absolute interpreter keeps the script reachable when a test
        # narrows PATH to prove what the hook finds on it.
        return subprocess.run(
            [sys.executable, str(self.root / ".agents/skills/cloud/scripts" / name), *args],
            cwd="/tmp", env=env, input='{"hook_event_name":"SessionStart"}',
            text=True, capture_output=True,
        )

    def path_without_devkit(self):
        """A PATH carrying what the hooks themselves run, and no devkit."""
        binaries = self.root / "path without devkit"
        binaries.mkdir(exist_ok=True)
        for name in ("git", "sh", "python3"):
            source = shutil.which(name)
            if source is None:
                self.fail(f"{name} is not on PATH")
            link = binaries / name
            if not link.exists():
                link.symlink_to(source)
        return str(binaries)

    def test_local_hooks_leave_checkout_alone(self):
        config = self.root / "devkit.local.toml"
        config.write_text("# local preferences\n")
        for name in ("cloud_setup.py", "cloud_startup.py", "cloud_compact.py"):
            with self.subTest(name=name):
                result = self.run_script(name)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertEqual(result.stderr, "")
        self.assertEqual(config.read_text(), "# local preferences\n")
        self.assertFalse((self.root / "AGENTS.local.md").exists())
        self.assertFalse((self.root / ".git/devkit-cloud-hooks").exists())

    def test_setup_generates_config_with_bundled_helper(self):
        result = self.run_script("cloud_setup.py", "--cloud", "--handoff")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(str(self.root / "AGENTS.local.md"), result.stdout)
        config = tomllib.loads((self.root / "devkit.local.toml").read_text())
        self.assertEqual(config["defaults"]["pr_create_state"], "draft")
        self.assertTrue(config["harness"]["enforce_commands"])
        self.assertTrue(config["harness"]["enforce_writes"])
        helper = Path(config["templates"]["variables"]["git_commit_patch"])
        self.assertEqual(helper, self.root / ".agents/skills/cloud/scripts/git-commit-patch.py")
        self.assertTrue(helper.is_file())
        self.assertEqual(config["templates"]["variables"]["human_tldr"], "")
        self.assertNotIn("/home/lev", json.dumps(config))
        self.assertIn(".agents/skills/cloud/SKILL.md", (self.root / "AGENTS.local.md").read_text())
        self.assertEqual((self.root / "CLAUDE.local.md").read_text(), "@AGENTS.local.md\n")
        self.assertEqual(self.run_script("cloud_setup.py", "--cloud").returncode, 0)
        self.assertEqual(tomllib.loads((self.root / "devkit.local.toml").read_text()), config)

    def test_startup_loads_workflow_without_network(self):
        result = self.run_script("cloud_startup.py", cloud=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("AGENTS.local.md", result.stdout)
        self.assertNotIn("Find existing work", result.stdout)
        self.assertIn("draft PR", (self.root / "AGENTS.local.md").read_text())
        self.assertTrue((self.root / "devkit.local.toml").exists())

    def test_settings_route_startup_and_recovery_from_another_directory(self):
        settings = json.loads((ROOT / ".claude/settings.json").read_text())
        env = dict(self.env, CLOUD_AGENT="true", CLAUDE_PROJECT_DIR=str(self.root))
        outputs = {}
        for source in ("startup", "clear", "fork", "resume", "compact"):
            entries = [entry for entry in settings["hooks"]["SessionStart"] if re.fullmatch(entry["matcher"], source)]
            self.assertEqual(len(entries), 1)
            command = entries[0]["hooks"][0]["command"]
            result = subprocess.run(command, shell=True, cwd="/tmp", env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            outputs[source] = result.stdout
        self.assertEqual(outputs["startup"], outputs["clear"])
        self.assertEqual(outputs["resume"], outputs["compact"])
        self.assertIn("AGENTS.local.md", outputs["startup"])
        self.assertIn("Continue", outputs["compact"])

    def test_compaction_only_emits_recovery(self):
        config = self.root / "devkit.local.toml"
        config.write_text("# session adjustment\n")
        result = self.run_script("cloud_compact.py", cloud=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(config.read_text(), "# session adjustment\n")
        self.assertIn("Continue", result.stdout)
        self.assertIn("unfinished", result.stdout)
        self.assertIn("approval", result.stdout)
        self.assertLess(len(result.stdout.split()), 150)
        self.assertFalse((self.root / "AGENTS.local.md").exists())

    @unittest.skipUnless(merge_tree_takes_trees(), "git merge-tree does not accept tree arguments")
    def test_devkit_commits_patch_and_preserves_unrelated_staging(self):
        self.assertEqual(self.run_script("cloud_setup.py", "--cloud").returncode, 0)
        env = dict(self.env, CLOUD_AGENT="true", DEVKIT_SKIP_AUTOLINK="1")

        def git(*args):
            return subprocess.run(
                ["git", "-C", str(self.root), *args],
                env=env, check=True, text=True, capture_output=True,
            ).stdout

        git("init", "-b", "test-cloud")
        git("config", "user.name", "Cloud Test")
        git("config", "user.email", "cloud@example.invalid")
        selected = self.root / "selected.txt"
        unrelated = self.root / "unrelated.txt"
        selected.write_text("before\n")
        unrelated.write_text("before\n")
        git("add", "--", "selected.txt", "unrelated.txt")
        git("commit", "-m", "test: initialize fixture\n\nCo-authored-by: Agent <agent@example.invalid>")
        selected.write_text("selected change\n")
        unrelated.write_text("unrelated change\n")
        git("add", "--", "unrelated.txt")
        patch = self.root / "selected.patch"
        patch.write_text(git("diff", "--", "selected.txt"))
        result = subprocess.run(
            ["devkit", "run", "-C", str(self.root), "--config", str(self.root / "devkit.local.toml"),
             "task", "commit-patch", "--arg", f"patch={patch}", "--arg", "commit_subject=fix: commit selected patch",
             "--arg", "coauthors=Agent <agent@example.invalid>"],
            env=env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(git("show", "HEAD:selected.txt"), "selected change\n")
        self.assertEqual(git("show", "HEAD:unrelated.txt"), "before\n")
        self.assertEqual(git("diff", "--cached", "--name-only").strip(), "unrelated.txt")
        self.assertEqual(git("diff", "--name-only"), "")

    def test_cloud_commit_hook_enforces_author_and_trailer(self):
        self.assertEqual(self.run_script("cloud_setup.py", "--cloud").returncode, 0)
        env = dict(self.env, CLOUD_AGENT="true")
        good = "test: cloud commit\n\nCo-authored-by: Agent <agent@example.invalid>"
        for args, error in ((["-m", "test: missing credit"], "Co-authored-by"),
                            (["--author", "Wrong <wrong@example.invalid>", "-m", good], "Cloud Test"),
                            (["-m", "test: empty credit\n\nCo-authored-by:"], "Co-authored-by")):
            with self.subTest(args=args):
                result = subprocess.run(
                    ["git", "-C", str(self.root), "commit", "--allow-empty", *args],
                    env=env, text=True, capture_output=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stderr)
        result = subprocess.run(
            ["git", "-C", str(self.root), "commit", "--allow-empty", "-m", good],
            env=env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_cloud_hook_preserves_existing_hooks_and_is_local(self):
        hooks = self.root / "existing hooks"
        hooks.mkdir()
        original = hooks / "commit-msg"
        original.write_text("#!/usr/bin/env python3\nimport sys\nprint('existing hook', file=sys.stderr)\n")
        original.chmod(0o755)
        pre_commit = hooks / "pre-commit"
        pre_commit.write_text("#!/usr/bin/env python3\nimport sys\nprint('existing pre-commit', file=sys.stderr)\n")
        pre_commit.chmod(0o755)
        subprocess.run(["git", "-C", str(self.root), "config", "core.hooksPath", str(hooks)], env=self.env, check=True)
        for _ in range(2):
            result = self.run_script("cloud_setup.py", "--cloud")
            self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run(
            ["git", "-C", str(self.root), "commit", "--allow-empty", "-m", "test: local commit"],
            env=self.env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr.count("existing hook"), 1)
        self.assertEqual(result.stderr.count("existing pre-commit"), 1)
        self.assertIn("existing hook", original.read_text())
        original.write_text(
            "#!/usr/bin/env python3\nimport sys\nprint('original rejection', file=sys.stderr)\nsys.exit(1)\n"
        )
        blocked = "test: blocked\n\nCo-authored-by: Agent <agent@example.invalid>"
        result = subprocess.run(
            ["git", "-C", str(self.root), "commit", "--allow-empty", "-m", blocked],
            env=dict(self.env, CLOUD_AGENT="true"), text=True, capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("original rejection", result.stderr)

    def test_cloud_setup_requires_expected_author(self):
        self.env.pop("GIT_AUTHOR_NAME")
        result = self.run_script("cloud_setup.py", "--cloud")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Set GIT_AUTHOR_NAME", result.stderr)
        self.assertFalse((self.root / ".git/devkit-cloud-hooks").exists())

    def fake_github(self, versions):
        """A file:// stand-in for the owner's GitHub: plugin manifests on the
        default branch, and an installer published only for each pinned release.
        Each installer drops a stub binary into its <APP>_INSTALL_DIR."""
        github = self.root / "fake github"
        manifests = {"devkit": ".claude-plugin/plugin.json", "mcpls": "plugin/.claude-plugin/plugin.json"}
        for app, version in versions.items():
            manifest = github / app / "raw/HEAD" / manifests[app]
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({"name": app, "version": version}))
            release = github / app / "releases/download" / f"v{version}"
            release.mkdir(parents=True)
            prefix = f"${app.upper()}_INSTALL_DIR"
            (release / f"{app}-installer.sh").write_text(
                f'mkdir -p "{prefix}/bin"\n'
                f"printf '#!/bin/sh\\necho \"$*\" >> \"$0.calls\"\\n' > \"{prefix}/bin/{app}\"\n"
                f'chmod +x "{prefix}/bin/{app}"\n'
            )
        return github.as_uri()

    def test_install_pins_releases_to_plugin_versions_and_stamps_bootstraps(self):
        versions = {"devkit": "9.1.2", "mcpls": "9.3.4"}
        home = self.root / "home"
        prefixes = {app: self.root / f"{app} prefix" for app in versions}
        result = self.run_script(
            "cloud_setup.py", "--cloud", "--install",
            CLOUD_SETUP_GITHUB=self.fake_github(versions), HOME=str(home), XDG_STATE_HOME=None,
            DEVKIT_INSTALL_DIR=str(prefixes["devkit"]), MCPLS_INSTALL_DIR=str(prefixes["mcpls"]),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((prefixes["devkit"] / "bin/devkit.calls").read_text(), "install-links\n")
        self.assertTrue((prefixes["mcpls"] / "bin/mcpls").is_file())
        for app, version in versions.items():
            stamp = home / ".local/state" / app / "bootstrap-version"
            self.assertEqual(stamp.read_text(), f"{version}\n")

    def test_install_requires_install_dirs(self):
        for missing in ("DEVKIT_INSTALL_DIR", "MCPLS_INSTALL_DIR"):
            with self.subTest(missing=missing):
                dirs = {name: None if name == missing else str(self.root / name)
                        for name in ("DEVKIT_INSTALL_DIR", "MCPLS_INSTALL_DIR")}
                result = self.run_script(
                    "cloud_setup.py", "--cloud", "--install",
                    CLOUD_SETUP_GITHUB=(self.root / "no github").as_uri(), **dirs,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(missing, result.stderr)

    def test_startup_names_the_harness_task_tools(self):
        result = self.run_script("cloud_startup.py", cloud=True, CLOUD_AGENT_TYPE="claude")
        self.assertEqual(result.returncode, 0, result.stderr)
        for tool in ("TaskCreate", "TaskUpdate", "TaskList", "TaskGet"):
            self.assertIn(tool, result.stdout)

    def test_startup_falls_back_to_generic_task_tools(self):
        for value in (None, "", "   ", "codex", "gemini"):
            with self.subTest(value=value):
                result = self.run_script("cloud_startup.py", cloud=True, CLOUD_AGENT_TYPE=value)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("task tracking tools", result.stdout)
                self.assertNotIn("TaskCreate", result.stdout)

    def test_startup_reads_the_agent_type_case_insensitively(self):
        result = self.run_script("cloud_startup.py", cloud=True, CLOUD_AGENT_TYPE="  Claude ")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("TaskCreate", result.stdout)

    def test_startup_ignores_the_agent_type_outside_the_cloud(self):
        result = self.run_script("cloud_startup.py", CLOUD_AGENT_TYPE="claude")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_startup_reports_installed_devkit(self):
        result = self.run_script("cloud_startup.py", cloud=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"devkit \d+\.\d+\.\d+ is on PATH")
        self.assertNotIn("--install", result.stdout)

    def test_startup_reports_missing_devkit_with_its_install_command(self):
        result = self.run_script("cloud_startup.py", cloud=True, PATH=self.path_without_devkit())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("issue", result.stdout)
        self.assertIn("cloud_setup.py --cloud --install", result.stdout)

    def test_startup_counts_devkit_that_resolves_but_does_not_run_as_missing(self):
        binaries = Path(self.path_without_devkit())
        for name in ("devkit", "issue", "devrun", "portm", "lockm", "docm", "devkit-mcp"):
            broken = binaries / name
            broken.write_text("#!/bin/sh\nexit 3\n")
            broken.chmod(0o755)
        result = self.run_script("cloud_startup.py", cloud=True, PATH=str(binaries))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cloud_setup.py --cloud --install", result.stdout)

    def test_compaction_reports_devkit_only_when_it_is_missing(self):
        missing = self.run_script("cloud_compact.py", cloud=True, PATH=self.path_without_devkit())
        self.assertEqual(missing.returncode, 0, missing.stderr)
        self.assertIn("cloud_setup.py --cloud --install", missing.stdout)
        self.assertIn("Continue", missing.stdout)
        self.assertLess(len(missing.stdout.split()), 150)
        present = self.run_script("cloud_compact.py", cloud=True)
        self.assertEqual(present.returncode, 0, present.stderr)
        self.assertNotIn("--install", present.stdout)


if __name__ == "__main__":
    unittest.main()
