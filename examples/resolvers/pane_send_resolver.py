#!/usr/bin/env python3
"""Reference pane-send resolver notification handler.

Wire this script as a ``[[notifications.handler]]`` for ``kind = ["waiting"]``.
Rimz invokes it once per notification with ``RIMZ_NOTIFY_REQUEST_ID``,
``RIMZ_NOTIFY_PANE``, and ``RIMZ_NOTIFY_ROOT``. The handler inspects the feed
item, captures the pane, matches only bounded prompt strings, sends the answer
through the pane, then records the outcome with ``rimz feed resolve``.

Captured pane text is untrusted data. This example treats it as bytes on a
screen, not instructions. Unknown shapes are answered by silence.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time

PROMPT_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"^Are you sure\? \[y/N\]\s*$"), "y\n"),
    (re.compile(r"^Do you want to continue\? \[y/N\]\s*$"), "y\n"),
    (re.compile(r"^Proceed\? \[Y/n\]\s*$"), "y\n"),
]


def run_rimz(rimz_bin: str, args: list[str]) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [rimz_bin, *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def load_item(rimz_bin: str, request_id: str) -> dict | None:
    result = run_rimz(rimz_bin, ["feed", "show", request_id, "--json"])
    if result.returncode != 0:
        print(
            f"[pane_send_resolver] feed show failed: "
            f"{result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return None
    try:
        item = json.loads(result.stdout or b"{}")
    except json.JSONDecodeError as exc:
        print(f"[pane_send_resolver] feed show parse: {exc}", file=sys.stderr)
        return None
    return item if isinstance(item, dict) else None


def item_pane(item: dict) -> str | None:
    pane = item.get("pane")
    if isinstance(pane, dict):
        pane_id = pane.get("pane_id")
        if isinstance(pane_id, str) and pane_id:
            return pane_id
    return None


def capture_pane(rimz_bin: str, pane_id: str) -> list[str] | None:
    result = run_rimz(rimz_bin, ["pane", "capture", pane_id, "--lines", "80", "--json"])
    if result.returncode != 0:
        print(
            f"[pane_send_resolver] pane capture failed: "
            f"{result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return None
    try:
        parsed = json.loads(result.stdout or b"{}")
    except json.JSONDecodeError:
        return None
    lines = parsed.get("lines")
    return [str(line) for line in lines] if isinstance(lines, list) else None


def match_prompt(lines: list[str]) -> str | None:
    for raw in reversed(lines):
        line = raw.rstrip()
        if not line:
            continue
        for pattern, response in PROMPT_PATTERNS:
            if pattern.match(line):
                return response
        return None
    return None


def handle_request(rimz_bin: str, request_id: str, pane_hint: str, by: str) -> int:
    item = load_item(rimz_bin, request_id)
    if item is None or item.get("status") != "pending":
        return 0

    pane_id = pane_hint or item_pane(item)
    if not pane_id:
        return 0

    lines = capture_pane(rimz_bin, pane_id)
    if lines is None:
        return 0

    response = match_prompt(lines)
    if response is None:
        return 0

    send = run_rimz(rimz_bin, ["pane", "send", pane_id, "--", response])
    if send.returncode != 0:
        print(
            f"[pane_send_resolver] pane send failed: "
            f"{send.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return 0

    time.sleep(0.2)
    _ = capture_pane(rimz_bin, pane_id)

    resolve = run_rimz(
        rimz_bin,
        [
            "feed",
            "resolve",
            request_id,
            "--decision",
            json.dumps({"choice": "yes"}),
            "--by",
            by,
            "--method",
            "pane-send",
        ],
    )
    if resolve.returncode != 0:
        print(
            f"[pane_send_resolver] resolve {request_id} failed: "
            f"{resolve.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rimz-bin", default=os.environ.get("RIMZ_BIN", "rimz"))
    parser.add_argument("--by", default="pane-send-resolver")
    parser.add_argument("--request-id", default=os.environ.get("RIMZ_NOTIFY_REQUEST_ID", ""))
    parser.add_argument("--pane", default=os.environ.get("RIMZ_NOTIFY_PANE", ""))
    return parser.parse_args(argv)


def main() -> int:
    args = parse_args()
    if not args.request_id:
        return 0
    return handle_request(args.rimz_bin, args.request_id, args.pane, args.by)


if __name__ == "__main__":
    raise SystemExit(main())
