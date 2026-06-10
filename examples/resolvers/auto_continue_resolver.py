#!/usr/bin/env python3
"""Reference auto-continue resolver for paused agent rows.

Polls ``rimz sidebar snapshot --json`` for rows whose enriched context says
the turn paused on provider overload. It then waits with bounded exponential
backoff and sends a human-authored recovery prompt through ``rimz steer``.

This is a reference artifact, not product. It proves the recovery loop can be
written outside Rimz using public commands.
"""

from __future__ import annotations

import argparse
import datetime as dt
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
RECOVERABLE_CAPTURE_PATTERNS = [
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        r"\bapi error\b",
        r"\boverloaded\b",
        r"\brate limit\b",
        r"\busage limit\b",
        r"\btemporar(?:y|ily) unavailable\b",
        r"\btry again\b",
        r"\b429\b",
    )
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
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="microseconds").replace(
        "+00:00", "Z"
    )


def write_heartbeat(workspace_id: str, resolver_id: str, display_name: str | None) -> None:
    atomic_write_json(
        heartbeat_path(workspace_id, resolver_id),
        {
            "protocol_version": PROTOCOL_VERSION,
            "workspace_id": workspace_id,
            "resolver_id": resolver_id,
            "display_name": display_name,
            "capabilities": ["sidebar.snapshot", "steer", "pane.capture", "pane.send"],
            "last_seen": _iso_now(),
            "version": RESOLVER_VERSION,
            "pid": os.getpid(),
        },
    )


def run_rimz(rimz_bin: str, args: list[str]) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [rimz_bin, *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def load_snapshot(args: argparse.Namespace) -> dict | None:
    if args.dry_run_snapshot:
        with open(args.dry_run_snapshot, "rb") as f:
            return json.load(f)
    result = run_rimz(
        args.rimz_bin,
        ["sidebar", "snapshot", "--workspace-id", args.workspace_id, "--json"],
    )
    if result.returncode != 0:
        print(
            f"[auto_continue_resolver] snapshot failed: {result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return None
    try:
        return json.loads(result.stdout or b"{}")
    except json.JSONDecodeError as exc:
        print(f"[auto_continue_resolver] snapshot parse: {exc}", file=sys.stderr)
        return None


def agent_rows(snapshot: dict) -> list[dict]:
    rows: list[dict] = []
    for group in snapshot.get("worktree_groups") or []:
        for row in group.get("rows") or []:
            if row.get("row_kind") == "agent":
                rows.append(row)
    return rows


def turn_error_class(row: dict) -> str | None:
    context = row.get("context") or {}
    error = context.get("turn_error") or {}
    value = error.get("class")
    return value if isinstance(value, str) else None


def pending_ask(row: dict) -> bool:
    return bool(row.get("request_id")) or row.get("status") == "waiting"


def rate_limit_wait_seconds(row: dict) -> float:
    context = row.get("context") or {}
    rate_limits = context.get("rate_limits") or {}
    windows = rate_limits.get("windows") or []
    waits: list[float] = []
    now = dt.datetime.now(dt.timezone.utc)
    for window in windows:
        resets_at = window.get("resets_at")
        if not isinstance(resets_at, str):
            continue
        try:
            reset = dt.datetime.fromisoformat(resets_at.replace("Z", "+00:00"))
        except ValueError:
            continue
        wait = (reset - now).total_seconds()
        if wait > 0:
            waits.append(wait)
    return min(waits) if waits else 0.0


def paused_candidates(snapshot: dict) -> list[dict]:
    candidates = []
    for row in agent_rows(snapshot):
        klass = turn_error_class(row)
        if row.get("status") != "paused" and klass not in (
            "paused_overloaded",
            "paused_rate_limit",
        ):
            continue
        if pending_ask(row):
            continue
        if klass == "paused_rate_limit" and rate_limit_wait_seconds(row) > 0:
            continue
        candidates.append(row)
    return candidates


def target_for(row: dict) -> str | None:
    pane = row.get("pane") or {}
    pane_id = pane.get("pane_id")
    if isinstance(pane_id, str) and pane_id:
        return pane_id
    row_id = row.get("id")
    return row_id if isinstance(row_id, str) and row_id else None


def capture_ok(rimz_bin: str, pane_id: str | None) -> bool:
    if not pane_id:
        return False
    result = run_rimz(rimz_bin, ["pane", "capture", pane_id, "--lines", "20", "--json"])
    if result.returncode != 0:
        return False
    try:
        capture = json.loads(result.stdout or b"{}")
    except json.JSONDecodeError:
        return False
    lines = capture.get("lines")
    if isinstance(lines, list):
        text = "\n".join(line for line in lines if isinstance(line, str))
    else:
        raw = capture.get("raw_text")
        text = raw if isinstance(raw, str) else ""
    return any(pattern.search(text) for pattern in RECOVERABLE_CAPTURE_PATTERNS)


def episode_key(row: dict) -> str:
    context = row.get("context") or {}
    error = context.get("turn_error") or {}
    parts = [
        str(target_for(row) or row.get("id") or ""),
        str(error.get("class") or ""),
        str(error.get("at") or ""),
        str(error.get("label") or ""),
    ]
    return "\0".join(parts)


def continue_row(args: argparse.Namespace, row: dict) -> bool:
    target = target_for(row)
    if not target:
        return False
    if args.dry_run_snapshot:
        print(f"would_continue {target}")
        return True
    pane_id = (row.get("pane") or {}).get("pane_id")
    if not capture_ok(args.rimz_bin, pane_id):
        return False
    if args.mode == "replay" and pane_id:
        result = run_rimz(args.rimz_bin, ["pane", "send", pane_id, "--key", "up", "--key", "enter"])
    else:
        result = run_rimz(args.rimz_bin, ["steer", target, "--", args.message])
    if result.returncode != 0:
        print(
            f"[auto_continue_resolver] continue {target} failed: {result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return False
    return True


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
    tick = max(args.tick_seconds, 0.2)
    deadline = None if args.run_seconds <= 0 else time.monotonic() + args.run_seconds
    attempts: dict[str, int] = {}
    next_attempt_at: dict[str, float] = {}
    while True:
        write_heartbeat(args.workspace_id, args.resolver_id, args.display_name)
        snapshot = load_snapshot(args)
        if snapshot is not None:
            active_keys: set[str] = set()
            for row in paused_candidates(snapshot):
                target = target_for(row)
                if not target:
                    continue
                key = episode_key(row)
                active_keys.add(key)
                count = attempts.get(key, 0)
                if count >= args.max_attempts:
                    continue
                now = time.monotonic()
                if not args.dry_run_snapshot and now < next_attempt_at.get(key, 0.0):
                    continue
                delay = min(args.max_backoff_seconds, 2 ** (count + 1))
                attempts[key] = count + 1
                next_attempt_at[key] = now + delay
                if continue_row(args, row):
                    pass
            for stale in set(attempts) - active_keys:
                attempts.pop(stale, None)
                next_attempt_at.pop(stale, None)
        if args.dry_run_snapshot:
            return
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
    parser.add_argument("--tick-seconds", type=float, default=2.0)
    parser.add_argument("--run-seconds", type=float, default=0.0)
    parser.add_argument("--max-attempts", type=int, default=3)
    parser.add_argument("--max-backoff-seconds", type=float, default=30.0)
    parser.add_argument("--message", default="continue")
    parser.add_argument("--mode", choices=["continue", "replay"], default="continue")
    parser.add_argument("--dry-run-snapshot", default=None)
    return parser.parse_args(argv)


def main() -> None:
    run_loop(parse_args())


if __name__ == "__main__":
    main()
