#!/usr/bin/env python3
"""Render a tag's changelog entry as Markdown for the GitHub release body.

Usage:
  release-notes.py v0.6.0 > release-notes.md

Reads `floppa-web-shared/src/changelog.json` — the same file the app shows in
"What's new" — so a release reads the same wherever it is read. CI pipes this
into `body_path`; `generate_release_notes` then appends the commit list below
it, which is the audience that wants one.

English only: a GitHub release page has no language switch, and the app is
where the Russian text is read. Exits non-zero if the tag has no entry, so a
release cannot quietly ship with an empty body.
"""

import json
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parent.parent / "floppa-web-shared" / "src" / "changelog.json"

# The four the app's own schema allows, in the order a reader wants them: what
# is new, what changed under them, what stopped being broken, then asides.
HEADINGS = {
    "added": "### Added",
    "changed": "### Changed",
    "fixed": "### Fixed",
    "notes": "### Notes",
}
ORDER = ["added", "changed", "fixed", "notes"]


def entry_for(data: dict, version: str) -> dict | None:
    """The entry for `version`, current or historical."""
    if data.get("version") == version:
        return data
    return next((h for h in data.get("history", []) if h.get("version") == version), None)


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(f"Usage: {sys.argv[0]} <tag>")

    # Tags are `vX.Y.Z`; the changelog stores the bare version.
    version = sys.argv[1].lstrip("v")
    data = json.loads(CHANGELOG.read_text())

    entry = entry_for(data, version)
    if entry is None:
        sys.exit(f"No changelog entry for {version} in {CHANGELOG.name}")

    sections = {s["type"]: s["items"] for s in entry.get("sections", [])}
    lines: list[str] = []
    for kind in ORDER:
        items = sections.get(kind)
        if not items:
            continue
        lines.append(HEADINGS[kind])
        lines.append("")
        # A stub the release was cut over is worth failing on rather than shipping.
        for item in items:
            text = item.get("en", "").strip()
            if text == "TODO":
                sys.exit(f"{version} still has a TODO in the {kind} section")
            lines.append(f"- {text}")
        lines.append("")

    if not lines:
        sys.exit(f"{version} has no sections to render")

    print("\n".join(lines).rstrip())


if __name__ == "__main__":
    main()
