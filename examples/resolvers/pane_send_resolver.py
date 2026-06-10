#!/usr/bin/env python3
"""Reference pane-send resolver — a test/doc artifact, not product.

Demonstrates the *universal answer surface* from
``docs/internals/agents/resolvers.md`` — wrapping any TTY prompt by capturing the
pane, matching the captured text against a **bounded** policy regex, typing
an answer through ``rimz pane send``, re-capturing to confirm, and finally
calling ``rimz feed resolve --method pane-send`` so the ledger reflects what
happened.

**Security discipline.** Captured pane text is untrusted data. A malicious
package can print arbitrary characters into the pane; a naive resolver that
pipes captured text into a model prompt as instructions will follow them.
This script matches only the exact strings in :data:`PROMPT_PATTERNS` and
abstains on anything else.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PROTOCOL_VERSION = "rimz.resolver.v1"
RESOLVER_VERSION = "0.1.0"

#: Bounded set of prompt regexes this resolver answers. Anything outside this
#: list abstains. Keep entries narrow — the discipline is "don't reach further
#: than the screen literally says".
PROMPT_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"^Are you sure\? \[y/N\]\s*$"), "y\n"),
    (re.compile(r"^Do you want to continue\? \[y/N\]\s*$"), "y\n"),
    (re.compile(r"^Proceed\? \[Y/n\]\s*$"), "y\n"),
]


def runtime_root() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime:
        return Path(runtime)
    return Path("/tmp") / f"rimz-{os.getuid()}"


def heartbeat_path(workspace_id: str, resolver_id: str) -> Path:
    return (
        runtime_root() / "rimz" / workspace_id / "heartbeat" / f"resolver.{resolver_id}.json"
    )


def atomic_write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_str = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    tmp = Path(tmp_str)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(payload, f)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise


def _iso_now() -> str:
    import datetime

    return (
        datetime.datetime.now(datetime.timezone.utc)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z")
    )


def write_heartbeat(
    workspace_id: str, resolver_id: str, display_name: str | None
) -> None:
    payload = {
        "protocol_version": PROTOCOL_VERSION,
        "workspace_id": workspace_id,
        "resolver_id": resolver_id,
        "display_name": display_name,
        "capabilities": ["pane.capture", "pane.send"],
        "last_seen": _iso_now(),
        "version": RESOLVER_VERSION,
        "pid": os.getpid(),
    }
    atomic_write_json(heartbeat_path(workspace_id, resolver_id), payload)


def run_rimz(rimz_bin: str, args: list[str]) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [rimz_bin, *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def list_pending_for(resolver_id: str, rimz_bin: str) -> list[dict]:
    result = run_rimz(rimz_bin, ["feed", "list", "--json"])
    if result.returncode != 0:
        print(
            f"[pane_send_resolver] feed list failed: "
            f"{result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return []
    try:
        items = json.loads(result.stdout or b"[]")
    except json.JSONDecodeError as exc:
        print(f"[pane_send_resolver] feed list parse: {exc}", file=sys.stderr)
        return []
    return [
        item
        for item in items
        if item.get("status") == "pending"
        and item.get("surface") == "bridge"
        and item.get("chain_active_resolver") == resolver_id
    ]


def capture_pane(pane_id: str, rimz_bin: str) -> list[str] | None:
    result = run_rimz(rimz_bin, ["pane", "capture", pane_id, "--lines", "80", "--json"])
    if result.returncode != 0:
        return None
    try:
        parsed = json.loads(result.stdout)
    except json.JSONDecodeError:
        return None
    lines = parsed.get("lines")
    return [str(line) for line in lines] if isinstance(lines, list) else None


def match_prompt(lines: list[str]) -> str | None:
    """Return the keystrokes to send when the pane's last non-empty line
    matches one of :data:`PROMPT_PATTERNS`. ``None`` means "no policy match,
    abstain".
    """
    for raw in reversed(lines):
        line = raw.rstrip()
        if not line:
            continue
        for pattern, response in PROMPT_PATTERNS:
            if pattern.match(line):
                return response
        return None
    return None


def handle_item(item: dict, resolver_id: str, rimz_bin: str) -> None:
    request_id = item["request_id"]
    pane = item.get("pane") or {}
    pane_id = pane.get("pane_id")
    if not pane_id:
        abstain(rimz_bin, request_id, resolver_id, "no_pane")
        return

    lines = capture_pane(pane_id, rimz_bin)
    if lines is None:
        abstain(rimz_bin, request_id, resolver_id, "pane_capture_failed")
        return

    response = match_prompt(lines)
    if response is None:
        abstain(rimz_bin, request_id, resolver_id, "pane_pattern_no_match")
        return

    send = run_rimz(rimz_bin, ["pane", "send", pane_id, "--", response])
    if send.returncode != 0:
        abstain(rimz_bin, request_id, resolver_id, "pane_send_failed")
        return

    # Brief settle then re-capture for confirmation. The pane primitives are
    # asynchronous; the discipline is "re-capture after sending".
    time.sleep(0.2)
    _ = capture_pane(pane_id, rimz_bin)

    resolve = run_rimz(
        rimz_bin,
        [
            "feed",
            "resolve",
            request_id,
            "--decision",
            json.dumps({"choice": "yes"}),
            "--resolver-id",
            resolver_id,
            "--method",
            "pane-send",
        ],
    )
    if resolve.returncode != 0:
        print(
            f"[pane_send_resolver] resolve {request_id} non-fatal: "
            f"{resolve.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )


def abstain(rimz_bin: str, request_id: str, resolver_id: str, reason: str) -> None:
    result = run_rimz(
        rimz_bin,
        [
            "feed",
            "abstain",
            request_id,
            "--resolver-id",
            resolver_id,
            "--reason",
            reason,
        ],
    )
    if result.returncode != 0:
        print(
            f"[pane_send_resolver] abstain {request_id} non-fatal: "
            f"{result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )


def install_signal_handlers(workspace_id: str, resolver_id: str) -> None:
    def _cleanup_and_exit(signum, _frame):
        try:
            heartbeat_path(workspace_id, resolver_id).unlink(missing_ok=True)
        finally:
            sys.exit(0 if signum in (signal.SIGTERM, signal.SIGINT) else 1)

    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, _cleanup_and_exit)


def run_loop(args: argparse.Namespace) -> None:
    install_signal_handlers(args.workspace_id, args.resolver_id)
    tick = max(args.tick_seconds, 0.05)
    deadline = None if args.run_seconds <= 0 else time.monotonic() + args.run_seconds
    while True:
        write_heartbeat(args.workspace_id, args.resolver_id, args.display_name)
        for item in list_pending_for(args.resolver_id, args.rimz_bin):
            handle_item(item, args.resolver_id, args.rimz_bin)
        if deadline is not None and time.monotonic() >= deadline:
            heartbeat_path(args.workspace_id, args.resolver_id).unlink(missing_ok=True)
            return
        time.sleep(tick)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--workspace-id", required=True, help="WorkspaceId (ws_...)")
    parser.add_argument("--resolver-id", required=True, help="Allowlist resolver id")
    parser.add_argument("--display-name", default=None)
    parser.add_argument("--rimz-bin", default="rimz", help="Path to the rimz binary")
    parser.add_argument(
        "--tick-seconds",
        type=float,
        default=1.0,
        help="Heartbeat + poll cadence (suggested 1s, TTL is 3s)",
    )
    parser.add_argument(
        "--run-seconds",
        type=float,
        default=0.0,
        help="Stop after this many seconds. 0 means run forever.",
    )
    return parser.parse_args(argv)


def main() -> None:
    run_loop(parse_args())


if __name__ == "__main__":
    main()
