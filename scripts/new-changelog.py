#!/usr/bin/env python3
"""Rotate changelog: move current version to history, add stub for new version."""

import json
import sys
from pathlib import Path

CHANGELOG_PATH = Path(__file__).resolve().parent.parent / "floppa-client" / "src" / "changelog.json"

STUB_SECTIONS = [
    {"type": "added", "items": [{"en": "TODO", "ru": "TODO"}]},
    {"type": "changed", "items": [{"en": "TODO", "ru": "TODO"}]},
    {"type": "fixed", "items": [{"en": "TODO", "ru": "TODO"}]},
]


def main():
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <new-version>")
        print(f"Example: {sys.argv[0]} 0.3.4")
        sys.exit(1)

    new_version = sys.argv[1]

    data = json.loads(CHANGELOG_PATH.read_text())

    # Extract current entry (without history)
    old_entry = {"version": data["version"], "sections": data["sections"]}

    # Build new changelog
    history = [old_entry, *data.get("history", [])]
    new_data = {
        "version": new_version,
        "sections": STUB_SECTIONS,
        "history": history,
    }

    CHANGELOG_PATH.write_text(json.dumps(new_data, indent=2, ensure_ascii=False) + "\n")
    print(f"Rotated changelog: {data['version']} -> history, new version: {new_version}")


if __name__ == "__main__":
    main()
