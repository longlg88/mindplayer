#!/usr/bin/env python3
"""Exercise the release calculator as a subprocess; fixtures stay in temp dirs."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / ".github" / "next-version.py"
RELEASE_MD = ROOT / "RELEASE.md"
EXISTING_OUTPUT = "existing=value\n"


class NextVersionTests(unittest.TestCase):
    def run_version(self, commits, current="0.57.2", rules=None, github_output=True):
        with tempfile.TemporaryDirectory(prefix="mindplayer-next-version-") as tmp:
            directory = Path(tmp)
            release_md = RELEASE_MD
            if rules is not None:
                release_md = directory / "RELEASE.md"
                release_md.write_text(rules, encoding="utf-8")
            output = directory / "github-output"
            output.write_text(EXISTING_OUTPUT, encoding="utf-8")
            env = os.environ.copy()
            env.pop("GITHUB_OUTPUT", None)
            if github_output:
                env["GITHUB_OUTPUT"] = str(output)
            result = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), str(release_md), current],
                input="\0".join(commits) + "\0",
                capture_output=True,
                text=True,
                env=env,
                cwd=directory,
                check=False,
            )
            return result, output.read_text(encoding="utf-8")

    def assert_version(self, commits, bump, version=None, **kwargs):
        result, output = self.run_version(commits, **kwargs)
        expected = f"bump={bump}\n"
        if version is not None:
            expected += f"version={version}\n"
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.stdout, expected)
        self.assertEqual(output, EXISTING_OUTPUT + expected)

    def assert_rejected(self, commits, reason, **kwargs):
        result, output = self.run_version(commits, **kwargs)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("Release-As", result.stderr)
        self.assertIn(reason, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(output, EXISTING_OUTPUT)

    def test_empty_and_nonrelease_ranges(self):
        for commits in (
            [],
            ["", " \n"],
            [f"{kind}(docs): update text" for kind in ("docs", "chore", "ci", "test", "style")],
            ["unclassified subject", "Merge branch 'topic'", "unknown: update"],
        ):
            with self.subTest(commits=commits):
                self.assert_version(commits, "none")

    def test_patch_types(self):
        for kind in ("fix", "perf", "refactor", "build", "revert", "deps", "patch"):
            with self.subTest(kind=kind):
                self.assert_version([f"{kind}(scope): change"], "patch", "0.57.3")

    def test_minor_types(self):
        for kind in ("feat", "feature", "minor", "FEAT"):
            with self.subTest(kind=kind):
                self.assert_version([f"{kind}: capability"], "minor", "0.58.0")

    def test_breaking_signals(self):
        for message in (
            "fix!: incompatible",
            "feat(api)!: incompatible",
            "chore: change\n\nBREAKING CHANGE: incompatible",
            "docs: change\n\nBREAKING-CHANGE: incompatible",
        ):
            with self.subTest(message=message):
                self.assert_version([message], "major", "1.0.0")

    def test_highest_bump_across_nul_separated_commits(self):
        self.assert_version(["fix: repair", "feat: add", "docs: text"], "minor", "0.58.0")
        self.assert_version(["feat: add", "fix!: incompatible"], "major", "1.0.0")

    def test_legacy_named_overrides(self):
        for override, version in (("patch", "0.57.3"), ("minor", "0.58.0"), ("major", "1.0.0")):
            with self.subTest(override=override):
                self.assert_version([f"chore: release\n\nRelease-As: {override}"], override, version)
        self.assert_version(["fix!: change\n\n release-as: PATCH \n"], "patch", "0.57.3")
        self.assert_version(["chore: release\n\nRelease-As: patch\nRelease-As: major"], "patch", "0.57.3")
        self.assert_version(["chore: release\n\nRelease-As: patch", "feat: add"], "minor", "0.58.0")

    def test_legacy_current_version_parsing(self):
        for current, version in (("v0.57.2", "0.57.3"), ("7", "7.0.1"), ("7.8", "7.8.1")):
            with self.subTest(current=current):
                self.assert_version(["fix: repair"], "patch", version, current=current)

    def test_rules_still_come_from_release_document(self):
        rules = "<!-- ci:bump-rules\nminor = enhance\npatch = repair\n-->"
        self.assert_version(["repair: change", "enhance: add"], "minor", "0.58.0", rules=rules)
        self.assert_version(["fix: repair"], "none", rules=rules)
        self.assert_version(["fix: repair"], "none", rules="no rules")
        self.assert_version(["fix!: change"], "major", "1.0.0", rules="no rules")

    def test_stdout_without_github_output(self):
        result, output = self.run_version(["fix: repair"], github_output=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "bump=patch\nversion=0.57.3\n")
        self.assertEqual(output, EXISTING_OUTPUT)

    def test_exact_patch_target(self):
        self.assert_version(["fix: repair\n\nRelease-As: 0.57.5"], "patch", "0.57.5")

    def test_exact_target_releases_chore_only_range(self):
        self.assert_version(["chore: release\n\nRelease-As: 0.57.5", "docs: text"], "patch", "0.57.5")

    def test_exact_target_can_equal_normal_minimum(self):
        for kind, bump, version in (("fix", "patch", "0.57.3"), ("feat", "minor", "0.58.0"), ("fix!", "major", "1.0.0")):
            with self.subTest(bump=bump):
                self.assert_version([f"{kind}: change\n\nRelease-As: {version}"], bump, version)

    def test_exact_target_reports_actual_bump(self):
        self.assert_version(["fix: repair\n\nRelease-As: 0.58.4"], "minor", "0.58.4")
        self.assert_version(["chore: release\n\nRelease-As: 2.1.3"], "major", "2.1.3")
        self.assert_version(["chore: release\n\nRelease-As: 0.10.0"], "minor", "0.10.0", current="0.9.9")

    def test_duplicate_exact_targets_are_allowed(self):
        message = "fix: repair\n\nRelease-As: 0.57.5\nRelease-As: 0.57.5"
        self.assert_version([message, "chore: release\n\nrelease-as: 0.57.5"], "patch", "0.57.5")

    def test_exact_trailer_whitespace_and_case(self):
        self.assert_version(["fix: repair\r\n\r\n\tRELEASE-AS:\t0.57.5 \r\n"], "patch", "0.57.5")

    def test_exact_target_must_meet_required_bump(self):
        for commits in (
            ["feat: add\n\nRelease-As: 0.57.5"],
            ["fix!: change\n\nRelease-As: 0.58.0"],
            ["chore: release\n\nRelease-As: 0.57.5", "feat: add"],
            ["fix: change\n\nBREAKING CHANGE: incompatible", "chore: release\n\nRelease-As: 0.99.0"],
            ["chore: release\n\nRelease-As: major\nRelease-As: 0.57.5"],
        ):
            with self.subTest(commits=commits):
                self.assert_rejected(commits, "required")

    def test_exact_and_named_overrides_can_coexist(self):
        self.assert_version(["chore: release\n\nRelease-As: minor\nRelease-As: 0.58.4"], "minor", "0.58.4")

    def test_exact_floor_uses_document_rules(self):
        self.assert_rejected(["api: change\n\nRelease-As: 0.57.5"], "required", rules="<!-- ci:bump-rules\nmajor = api\n-->")

    def test_exact_target_must_be_strictly_newer(self):
        for version in ("0.57.2", "0.57.1", "0.56.99", "0.0.0"):
            with self.subTest(version=version):
                self.assert_rejected([f"chore: release\n\nRelease-As: {version}"], "newer")

    def test_conflicting_exact_targets(self):
        for commits in (
            ["fix: repair\n\nRelease-As: 0.57.5\nRelease-As: 0.57.6"],
            ["fix: repair\n\nRelease-As: 0.57.5", "chore: release\n\nRelease-As: 0.57.6"],
        ):
            with self.subTest(commits=commits):
                self.assert_rejected(commits, "conflicting")

    def test_invalid_exact_targets(self):
        for value in (
            "", "none", "later", "v0.57.5", "0.57", "0.57.5.1", "0.57.5-beta.1", "0.57.5+build.1",
            "00.57.5", "0.057.5", "0.57.05", "+0.57.5", "-1.0.0", "0.57.-5", "0. 57.5",
            "0.57.5 extra", "0.57.5 0.57.6", "0.57.\n5", "０.57.5",
        ):
            with self.subTest(value=value):
                self.assert_rejected([f"fix: repair\n\nRelease-As: {value}"], "invalid")

    def test_valid_target_does_not_mask_invalid_trailer(self):
        self.assert_rejected(["fix: repair\n\nRelease-As: 0.57.5\nRelease-As: v0.57.5"], "invalid")


if __name__ == "__main__":
    unittest.main()
