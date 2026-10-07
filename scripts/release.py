#!/usr/bin/env python3
"""Release helper: bump every version in the workspace in one go.

    python scripts/release.py 0.2.0              # bump + roll the changelogs
    python scripts/release.py 0.2.0 --dry-run    # show what would change
    python scripts/release.py --check            # all versions agree? (CI)
    python scripts/release.py --notes 0.2.0      # print that version's changelog section

The script edits files only. It never commits, tags, pushes or publishes: after
a bump it prints the git commands to review and run yourself. Pushing the tag
is what triggers `.github/workflows/release.yml`.

Version locations kept in sync:
  * Cargo.toml                       [workspace.package] version
  * tpt-log-*/Cargo.toml             `version = "X"` on tpt-log-core/-transport path deps
  * tpt-log-wasm/package.json        "version"
  * tpt-log-py/python/tpt_microlog/__init__.py   __version__
  * tpt-log-go/README.md             the `-version vX` fetch example
  * Cargo.lock                       refreshed with `cargo update --workspace --offline`
Changelogs (CHANGELOG.md and tpt-log-*/CHANGELOG.md): the `## [Unreleased]`
section becomes `## [X] - <date>` and a fresh empty Unreleased is left on top.
"""

from __future__ import annotations

import argparse
import datetime
import json
import re
import subprocess
import sys
from pathlib import Path

SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$")
UNRELEASED = "## [Unreleased]"


class ReleaseError(Exception):
    pass


def parse_version(text: str) -> tuple:
    m = SEMVER.match(text)
    if not m:
        raise ReleaseError(f"{text!r} is not a semantic version (expected X.Y.Z or X.Y.Z-pre)")
    major, minor, patch, pre = m.groups()
    # A release outranks its own pre-releases (1.0.0 > 1.0.0-rc.1).
    return (int(major), int(minor), int(patch), 1 if pre is None else 0, pre or "")


# ---------------------------------------------------------------- file access
def read(path: Path) -> tuple[str, str]:
    """(text with \\n newlines, original newline style)"""
    raw = path.read_bytes().decode("utf-8")
    nl = "\r\n" if "\r\n" in raw else "\n"
    return raw.replace("\r\n", "\n"), nl


def write(path: Path, text: str, nl: str) -> None:
    path.write_bytes(text.replace("\n", nl).encode("utf-8"))


def crate_manifests(root: Path) -> list[Path]:
    return sorted(p for p in root.glob("tpt-log-*/Cargo.toml"))


def changelogs(root: Path) -> list[Path]:
    found = [root / "CHANGELOG.md"] + sorted(root.glob("tpt-log-*/CHANGELOG.md"))
    return [p for p in found if p.exists()]


# ----------------------------------------------------------------- versions
WORKSPACE_VERSION = re.compile(r'(?ms)(\[workspace\.package\].*?^version\s*=\s*")([^"]+)(")')
PATH_DEP = re.compile(
    r'(?m)^(\s*tpt-log-(?:core|transport)\s*=\s*\{[^}\n]*?\bpath\s*=\s*"[^"]+"[^}\n]*?\bversion\s*=\s*")([^"]+)(")'
)
PY_VERSION = re.compile(r'(?m)^(__version__\s*=\s*")([^"]+)(")')
GO_FETCH = re.compile(r"(fetchlib@[^\s]+ -version v)(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)")


def current_versions(root: Path) -> dict[str, list[str]]:
    """Every version found, keyed by location (for --check and reporting)."""
    found: dict[str, list[str]] = {}
    text, _ = read(root / "Cargo.toml")
    m = WORKSPACE_VERSION.search(text)
    if not m:
        raise ReleaseError("Cargo.toml has no [workspace.package] version")
    found["Cargo.toml [workspace.package]"] = [m.group(2)]
    for manifest in crate_manifests(root):
        deps = [d.group(2) for d in PATH_DEP.finditer(read(manifest)[0])]
        if deps:
            found[f"{manifest.relative_to(root).as_posix()} (path deps)"] = deps
    pkg = root / "tpt-log-wasm" / "package.json"
    if pkg.exists():
        found["tpt-log-wasm/package.json"] = [json.loads(read(pkg)[0])["version"]]
    init = root / "tpt-log-py" / "python" / "tpt_microlog" / "__init__.py"
    if init.exists():
        m = PY_VERSION.search(read(init)[0])
        if m:
            found["tpt-log-py __init__.py"] = [m.group(2)]
    return found


def check(root: Path) -> list[str]:
    versions = current_versions(root)
    expected = versions["Cargo.toml [workspace.package]"][0]
    problems = []
    for where, values in versions.items():
        for value in values:
            if value != expected:
                problems.append(f"{where}: {value} (workspace is {expected})")
    return problems


# --------------------------------------------------------------- changelogs
def roll_changelog(text: str, version: str, date: str, require_content: bool) -> str:
    if UNRELEASED not in text:
        raise ReleaseError("changelog has no '## [Unreleased]' section")
    head, _, rest = text.partition(UNRELEASED)
    # The Unreleased body runs to the next version heading (or the end).
    m = re.search(r"(?m)^## \[", rest)
    body, tail = (rest[: m.start()], rest[m.start():]) if m else (rest, "")
    if require_content and not body.strip():
        raise ReleaseError("the [Unreleased] section is empty: nothing to release")
    if f"## [{version}]" in text:
        raise ReleaseError(f"changelog already has a [{version}] section")
    body = body.strip("\n")
    section = f"## [{version}] - {date}\n\n{body}\n\n" if body else f"## [{version}] - {date}\n\n"
    return f"{head}{UNRELEASED}\n\n{section}{tail}"


def notes_for(text: str, version: str) -> str:
    m = re.search(rf"(?m)^## \[{re.escape(version)}\][^\n]*\n", text)
    if not m:
        raise ReleaseError(f"no [{version}] section in the changelog")
    rest = text[m.end():]
    nxt = re.search(r"(?m)^## \[", rest)
    return (rest[: nxt.start()] if nxt else rest).strip() + "\n"


# --------------------------------------------------------------------- bump
def sub_versions(text: str, pattern: re.Pattern, new: str) -> tuple[str, int]:
    return pattern.subn(lambda m: f"{m.group(1)}{new}{m.group(3)}", text)


def bump(root: Path, new: str, date: str, dry_run: bool, update_lock: bool, allow_empty: bool) -> list[str]:
    old = current_versions(root)["Cargo.toml [workspace.package]"][0]
    if parse_version(new) <= parse_version(old):
        raise ReleaseError(f"{new} is not newer than the current version {old}")
    problems = check(root)
    if problems:
        raise ReleaseError("versions disagree before the bump:\n  " + "\n  ".join(problems))

    plan: dict[Path, tuple[str, str]] = {}
    report: list[str] = []

    def stage(path: Path, text: str, nl: str, what: str) -> None:
        plan[path] = (text, nl)
        report.append(f"{path.relative_to(root).as_posix()}: {what}")

    text, nl = read(root / "Cargo.toml")
    new_text = WORKSPACE_VERSION.sub(lambda m: f"{m.group(1)}{new}{m.group(3)}", text, count=1)
    stage(root / "Cargo.toml", new_text, nl, f"workspace version {old} -> {new}")

    for manifest in crate_manifests(root):
        text, nl = read(manifest)
        new_text, n = sub_versions(text, PATH_DEP, new)
        if n:
            stage(manifest, new_text, nl, f"{n} path dependency version(s)")

    pkg = root / "tpt-log-wasm" / "package.json"
    if pkg.exists():
        text, nl = read(pkg)
        new_text, n = re.subn(r'(?m)^(\s*"version"\s*:\s*")[^"]+(")', lambda m: f"{m.group(1)}{new}{m.group(2)}", text, count=1)
        if n:
            stage(pkg, new_text, nl, f'"version" -> {new}')

    init = root / "tpt-log-py" / "python" / "tpt_microlog" / "__init__.py"
    if init.exists():
        text, nl = read(init)
        new_text, n = sub_versions(text, PY_VERSION, new)
        if n:
            stage(init, new_text, nl, f"__version__ -> {new}")

    go_readme = root / "tpt-log-go" / "README.md"
    if go_readme.exists():
        text, nl = read(go_readme)
        new_text, n = GO_FETCH.subn(lambda m: f"{m.group(1)}{new}", text)
        if n:
            stage(go_readme, new_text, nl, f"fetch example -> v{new}")

    for log in changelogs(root):
        text, nl = read(log)
        is_root = log == root / "CHANGELOG.md"
        stage(log, roll_changelog(text, new, date, require_content=is_root and not allow_empty), nl, f"[Unreleased] -> [{new}] - {date}")

    if not dry_run:
        for path, (text, nl) in plan.items():
            write(path, text, nl)
        if update_lock:
            result = subprocess.run(
                ["cargo", "update", "--workspace", "--offline"],
                cwd=root, capture_output=True, text=True,
            )
            report.append("Cargo.lock: refreshed" if result.returncode == 0
                          else f"Cargo.lock: NOT refreshed ({result.stderr.strip().splitlines()[-1] if result.stderr else 'cargo failed'}); run `cargo update --workspace`")
    return report


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("version", nargs="?", help="new version, e.g. 0.2.0")
    ap.add_argument("--dry-run", action="store_true", help="report changes without writing")
    ap.add_argument("--check", action="store_true", help="verify all versions agree")
    ap.add_argument("--notes", metavar="VERSION", help="print the changelog section for VERSION")
    ap.add_argument("--date", default=datetime.date.today().isoformat())
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    ap.add_argument("--no-lock", action="store_true", help="skip refreshing Cargo.lock")
    ap.add_argument("--allow-empty", action="store_true", help="allow releasing an empty [Unreleased]")
    args = ap.parse_args(argv)
    root = Path(args.root)
    try:
        if args.notes:
            sys.stdout.write(notes_for(read(root / "CHANGELOG.md")[0], args.notes))
            return 0
        if args.check:
            problems = check(root)
            if problems:
                print("version mismatch:\n  " + "\n  ".join(problems), file=sys.stderr)
                return 1
            print("all versions agree:", current_versions(root)["Cargo.toml [workspace.package]"][0])
            return 0
        if not args.version:
            ap.error("a version is required (or use --check / --notes)")
        if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", args.date):
            raise ReleaseError("--date must be YYYY-MM-DD")
        report = bump(root, args.version, args.date, args.dry_run, not args.no_lock, args.allow_empty)
    except ReleaseError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    print(("DRY RUN, nothing written:\n  " if args.dry_run else "updated:\n  ") + "\n  ".join(report))
    if not args.dry_run:
        v = args.version
        print(
            f"\nNext (review, then run yourself; this script never commits, tags or publishes):\n"
            f"  git diff\n  cargo test --workspace --exclude tpt-log-py\n"
            f"  git commit -am \"release: v{v}\"\n  git tag v{v}\n"
            f"  git tag tpt-log-go/v{v}   # the Go module lives in a subdirectory, so it needs its own tag\n"
            f"  git push origin master v{v} tpt-log-go/v{v}   # v{v} triggers release.yml"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
