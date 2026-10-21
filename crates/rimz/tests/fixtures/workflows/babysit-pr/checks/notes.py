# /// script
# requires-python = ">=3.11"
# ///
"""Classify the open-fix entries without paying a judge for known shapes."""

import json
import pathlib
import re

path = pathlib.Path(".rimz/team/reflect-notes.md")
entries = []
section = None
if path.exists():
    for line in path.read_text().splitlines():
        if line.startswith("## "):
            section = line[3:].strip()
        elif section in {"Backlog", "User"} and line.startswith("### "):
            heading = line[4:].strip()
            if not re.search(r"\[[^\]]+\]$", heading):
                entries.append(heading)
outcome = "none" if not entries else "needed" if all(re.match(r"(Bug fix|Test|Feature|Refactor|Docs):", entry) for entry in entries) else "unsure"
print(json.dumps({"outcome": outcome, "reason": f"{len(entries)} unruled entries in .rimz/team/reflect-notes.md"}))
