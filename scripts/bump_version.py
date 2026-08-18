#!/usr/bin/env python3
"""Bump the single source of truth for the release version.

Usage::

    python scripts/bump_version.py 0.3.0     # bump + open a CHANGELOG section
    python scripts/bump_version.py --show    # print the current version
"""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CARGO = ROOT / "Cargo.toml"
CHANGELOG = ROOT / "CHANGELOG.md"

SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")

# Anchored to the [workspace.package] table so a dependency's `version = ...`
# can never be rewritten by accident.
VERSION_LINE = re.compile(
    r"(?P<head>\[workspace\.package\][^\[]*?\bversion\s*=\s*\")(?P<version>[^\"]+)(?P<tail>\")",
    re.DOTALL,
)
# `imgdl-core` is also referenced by version in [workspace.dependencies] so the
# crate stays publishable to crates.io; keep it in lockstep.
DEP_LINE = re.compile(
    r"(?P<head>imgdl-core\s*=\s*\{[^}]*?\bversion\s*=\s*\")(?P<version>[^\"]+)(?P<tail>\")"
)


def current_version() -> str:
    # Read with the same anchored regex used to write, rather than tomllib:
    # tomllib is 3.11+, and this package supports 3.10.
    match = VERSION_LINE.search(CARGO.read_text())
    if match is None:
        sys.exit("error: could not find [workspace.package] version in Cargo.toml")
    return match.group("version")


def bump(new: str) -> None:
    if not SEMVER.match(new):
        sys.exit(f"error: {new!r} is not a semver version (expected e.g. 0.3.0)")

    old = current_version()
    if old == new:
        sys.exit(f"error: version is already {new}")

    text = CARGO.read_text()
    text, n = VERSION_LINE.subn(rf"\g<head>{new}\g<tail>", text, count=1)
    if n != 1:
        sys.exit("error: could not find [workspace.package] version in Cargo.toml")
    text, _ = DEP_LINE.subn(rf"\g<head>{new}\g<tail>", text, count=1)
    CARGO.write_text(text)
    print(f"Cargo.toml: {old} -> {new}")

    _open_changelog_section(new)
    print("\nNext:")
    print(f"  git add -A && git commit -m 'chore: release v{new}'")
    print("  just tag && git push --follow-tags")


def _open_changelog_section(new: str) -> None:
    if not CHANGELOG.is_file():
        return
    today = dt.datetime.now(tz=dt.timezone.utc).date().isoformat()
    text = CHANGELOG.read_text()
    marker = "<!-- next-release -->"
    if marker not in text:
        print("CHANGELOG.md: no <!-- next-release --> marker, skipping", file=sys.stderr)
        return
    text = text.replace(marker, f"{marker}\n\n## [{new}] - {today}", 1)
    CHANGELOG.write_text(text)
    print(f"CHANGELOG.md: opened section for {new}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?", help="new version, e.g. 0.3.0")
    parser.add_argument("--show", action="store_true", help="print current version and exit")
    args = parser.parse_args(argv)

    if args.show or not args.version:
        print(current_version())
        return 0
    bump(args.version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
