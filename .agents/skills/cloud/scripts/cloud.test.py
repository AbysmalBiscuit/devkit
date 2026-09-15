import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[4]


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

    def run_script(self, name, *args, cloud=False):
        env = self.env.copy()
        if cloud:
            env["CLOUD_AGENT"] = "true"
        return subprocess.run(
            ["python3", str(self.root / ".agents/skills/cloud/scripts" / name), *args],
            cwd="/tmp", env=env, input='{"hook_event_name":"SessionStart"}',
            text=True, capture_output=True,
        )

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

    def test_setup_generates_config_with_bundled_helper(self):
        result = self.run_script("cloud_setup.py", "--cloud", "--handoff")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(str(self.root / ".agents/skills/cloud/SKILL.md"), result.stdout)
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
        self.assertIn("Find existing work", result.stdout)
        self.assertIn("draft PR", result.stdout)
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
        self.assertLess(len(outputs["compact"]), len(outputs["startup"]))

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

    def test_devkit_commits_patch_and_preserves_unrelated_staging(self):
        self.assertEqual(self.run_script("cloud_setup.py", "--cloud").returncode, 0)
        env = dict(self.env, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1", DEVKIT_SKIP_AUTOLINK="1")

        def git(*args):
            return subprocess.run(["git", "-C", str(self.root), *args], env=env, check=True, text=True, capture_output=True).stdout

        git("init", "-b", "test-cloud")
        git("config", "user.name", "Cloud Test")
        git("config", "user.email", "cloud@example.invalid")
        selected = self.root / "selected.txt"
        unrelated = self.root / "unrelated.txt"
        selected.write_text("before\n")
        unrelated.write_text("before\n")
        git("add", "--", "selected.txt", "unrelated.txt")
        git("commit", "-m", "test: initialize fixture")
        selected.write_text("selected change\n")
        unrelated.write_text("unrelated change\n")
        git("add", "--", "unrelated.txt")
        patch = self.root / "selected.patch"
        patch.write_text(git("diff", "--", "selected.txt"))
        result = subprocess.run(
            ["devkit", "run", "-C", str(self.root), "--config", str(self.root / "devkit.local.toml"),
             "task", "commit-patch", "--arg", f"patch={patch}", "--arg", "commit_subject=fix: commit selected patch"],
            env=env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(git("show", "HEAD:selected.txt"), "selected change\n")
        self.assertEqual(git("show", "HEAD:unrelated.txt"), "before\n")
        self.assertEqual(git("diff", "--cached", "--name-only").strip(), "unrelated.txt")
        self.assertEqual(git("diff", "--name-only"), "")


if __name__ == "__main__":
    unittest.main()
