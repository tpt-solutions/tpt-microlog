"""Tests for scripts/release.py (run: python scripts/test_release.py).

They bump a *copy* of the real repository files, so a manifest format change
that the script no longer understands fails here before it fails a release.
"""

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
SCRIPT = HERE / "release.py"

FILES = [
    "Cargo.toml",
    "CHANGELOG.md",
    "tpt-log-wasm/package.json",
    "tpt-log-py/python/tpt_microlog/__init__.py",
    "tpt-log-go/README.md",
]


def tracked_manifests():
    out = []
    for p in sorted(REPO.glob("tpt-log-*/Cargo.toml")):
        out.append(p.relative_to(REPO).as_posix())
    for p in sorted(REPO.glob("tpt-log-*/CHANGELOG.md")):
        out.append(p.relative_to(REPO).as_posix())
    return out


def run(root, *args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root), *args],
        capture_output=True, text=True,
    )


class ReleaseScript(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        for rel in FILES + tracked_manifests():
            dest = self.tmp / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / rel, dest)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def current(self):
        r = run(self.tmp, "--check")
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.split(":")[1].strip()

    def test_repo_versions_agree(self):
        self.assertEqual(run(REPO, "--check").returncode, 0, run(REPO, "--check").stderr)

    def test_bump_updates_every_location_and_rolls_changelogs(self):
        old = self.current()
        r = run(self.tmp, "9.8.7", "--no-lock", "--date", "2030-01-02")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.current(), "9.8.7")
        # Nothing mentions the old version where it is supposed to track it.
        for rel in tracked_manifests():
            if rel.endswith("Cargo.toml"):
                text = (self.tmp / rel).read_text(encoding="utf-8")
                self.assertNotIn(f'version = "{old}"', text, rel)
        self.assertIn('"version": "9.8.7"', (self.tmp / "tpt-log-wasm/package.json").read_text(encoding="utf-8"))
        self.assertIn('__version__ = "9.8.7"', (self.tmp / "tpt-log-py/python/tpt_microlog/__init__.py").read_text(encoding="utf-8"))
        self.assertIn("-version v9.8.7", (self.tmp / "tpt-log-go/README.md").read_text(encoding="utf-8"))
        changelog = (self.tmp / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertIn("## [Unreleased]\n\n## [9.8.7] - 2030-01-02\n", changelog.replace("\r\n", "\n"))
        # The release notes come out of the rolled section.
        notes = run(self.tmp, "--notes", "9.8.7")
        self.assertEqual(notes.returncode, 0)
        self.assertIn("Added", notes.stdout)
        self.assertNotIn("## [", notes.stdout)

    def test_dry_run_writes_nothing(self):
        before = {rel: (self.tmp / rel).read_bytes() for rel in FILES + tracked_manifests()}
        r = run(self.tmp, "9.8.7", "--dry-run", "--no-lock")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("DRY RUN", r.stdout)
        for rel, data in before.items():
            self.assertEqual((self.tmp / rel).read_bytes(), data, rel)

    def test_refuses_bad_versions(self):
        old = self.current()
        for bad in ("1.2", "v1.2.3", "banana", old, "0.0.1"):
            r = run(self.tmp, bad, "--no-lock")
            self.assertEqual(r.returncode, 2, f"{bad}: {r.stdout}")
            self.assertEqual(self.current(), old, "a refused bump must change nothing")

    def test_prerelease_ordering(self):
        r = run(self.tmp, "9.0.0-rc.1", "--no-lock")
        self.assertEqual(r.returncode, 0, r.stderr)
        # The final release outranks its release candidate.
        r = run(self.tmp, "9.0.0", "--no-lock", "--allow-empty")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_empty_unreleased_is_refused(self):
        self.assertEqual(run(self.tmp, "9.8.7", "--no-lock").returncode, 0)
        r = run(self.tmp, "9.8.8", "--no-lock")
        self.assertEqual(r.returncode, 2)
        self.assertIn("nothing to release", r.stderr)
        self.assertEqual(run(self.tmp, "9.8.8", "--no-lock", "--allow-empty").returncode, 0)

    def test_check_detects_drift(self):
        pkg = self.tmp / "tpt-log-wasm/package.json"
        pkg.write_text(pkg.read_text(encoding="utf-8").replace(f'"version": "{self.current()}"', '"version": "0.0.9"'), encoding="utf-8")
        r = run(self.tmp, "--check")
        self.assertEqual(r.returncode, 1)
        self.assertIn("package.json", r.stderr)
        self.assertEqual(run(self.tmp, "9.8.7", "--no-lock").returncode, 2, "no bump on drifted versions")

    def test_crlf_files_keep_their_line_endings(self):
        for rel in ("Cargo.toml", "CHANGELOG.md"):
            p = self.tmp / rel
            p.write_bytes(p.read_bytes().replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))
        self.assertEqual(run(self.tmp, "9.8.7", "--no-lock").returncode, 0)
        for rel in ("Cargo.toml", "CHANGELOG.md"):
            data = (self.tmp / rel).read_bytes()
            self.assertNotIn(b"\n", data.replace(b"\r\n", b""), f"{rel} has bare LF after the bump")


if __name__ == "__main__":
    unittest.main(verbosity=2)
