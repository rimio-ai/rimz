#!/usr/bin/env python3
"""Reference hook-bridge resolver — a test/doc artifact, not product.

Implements the public resolver protocol from
``docs/internals/resolvers.md``:

* Writes a heartbeat file at ``$XDG_RUNTIME_DIR/rimz/<ws>/heartbeat/
  resolver.<id>.json`` once per tick.
* Polls ``rimz feed list --json`` for bridge-surface items whose
  ``chain_active_resolver`` matches this resolver's id.
* For each pending item, runs a hard-coded policy: allow when the agent
  reports a ``tool_name`` in :data:`ALLOW_TOOLS`, abstain otherwise. The
  policy is intentionally minimal — real resolvers wrap a model or an
  organization policy, not a static set.

The resolver is stateless across ticks. It is safe to kill at any time;
``SIGTERM``/``SIGINT`` remove the heartbeat file so the chain advances on the
next bridge tick.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PROTOCOL_VERSION = "rimz.resolver.v1"
RESOLVER_VERSION = "0.1.0"

#: Built-in policy: which agent tools this resolver answers ``allow`` for.
#: Anything outside this set abstains and the chain advances. Real resolvers
#: derive this list from policy or a model call; we keep it static so the
#: example stays readable. Capitalized names are Claude's; the lowercase set
#: is pi's (its wire reports ``read``/``grep``/``find``/``ls``).
ALLOW_TOOLS = frozenset({"Read", "Grep", "Glob", "LS", "read", "grep", "find", "ls"})


def runtime_root() -> Path:
    """Resolve ``$XDG_RUNTIME_DIR`` with the same fallback the Rust side uses.

    The Rust ``ledger::paths`` falls back to ``/tmp/rimz-<uid>`` when
    ``XDG_RUNTIME_DIR`` is unset; we mirror it so this script works under
    the same conditions.
    """
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime:
        return Path(runtime)
    return Path("/tmp") / f"rimz-{os.getuid()}"


def heartbeat_path(workspace_id: str, resolver_id: str) -> Path:
    return (
        runtime_root() / "rimz" / workspace_id / "heartbeat" / f"resolver.{resolver_id}.json"
    )


def atomic_write_json(path: Path, payload: dict) -> None:
    """Temp-file + rename. Mirrors ``ledger::atomic::write_temp_then_rename``."""
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


def write_heartbeat(
    workspace_id: str, resolver_id: str, display_name: str | None
) -> None:
    payload = {
        "protocol_version": PROTOCOL_VERSION,
        "workspace_id": workspace_id,
        "resolver_id": resolver_id,
        "display_name": display_name,
        "capabilities": ["permission", "plan", "question"],
        "last_seen": _iso_now(),
        "version": RESOLVER_VERSION,
        "pid": os.getpid(),
    }
    atomic_write_json(heartbeat_path(workspace_id, resolver_id), payload)


def _iso_now() -> str:
    """RFC 3339 / ISO 8601 timestamp jiff parses as ``Timestamp``."""
    import datetime

    return (
        datetime.datetime.now(datetime.timezone.utc)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z")
    )


def run_rimz(rimz_bin: str, args: list[str]) -> subprocess.CompletedProcess[bytes]:
    """Wrap ``subprocess.run`` so every call shares stderr handling."""
    return subprocess.run(
        [rimz_bin, *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def list_pending_for(resolver_id: str, rimz_bin: str) -> list[dict]:
    """Return the items currently waiting on this resolver's chain slot."""
    result = run_rimz(rimz_bin, ["feed", "list", "--json"])
    if result.returncode != 0:
        print(
            f"[hook_bridge_resolver] feed list failed: "
            f"{result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )
        return []
    try:
        items = json.loads(result.stdout or b"[]")
    except json.JSONDecodeError as exc:
        print(f"[hook_bridge_resolver] feed list parse: {exc}", file=sys.stderr)
        return []
    return [
        item
        for item in items
        if item.get("status") == "pending"
        and item.get("surface") == "bridge"
        and item.get("chain_active_resolver") == resolver_id
    ]


def decide(item: dict) -> tuple[str, dict | str]:
    """Run the built-in policy.

    Returns ``("resolve", decision_payload)`` or ``("abstain", reason)``.
    """
    if item.get("kind") != "permission":
        return ("abstain", "kind_not_permission")
    tool_name = (item.get("payload") or {}).get("tool_name")
    if isinstance(tool_name, str) and tool_name in ALLOW_TOOLS:
        return ("resolve", {"choice": "allow"})
    return ("abstain", "tool_not_in_allowlist")


def apply_decision(
    item: dict, decision: tuple[str, dict | str], resolver_id: str, rimz_bin: str
) -> None:
    request_id = item["request_id"]
    kind, payload = decision
    if kind == "resolve":
        assert isinstance(payload, dict)
        result = run_rimz(
            rimz_bin,
            [
                "feed",
                "resolve",
                request_id,
                "--decision",
                json.dumps(payload),
                "--resolver-id",
                resolver_id,
                "--method",
                "hook-bridge",
            ],
        )
    else:
        assert isinstance(payload, str)
        result = run_rimz(
            rimz_bin,
            [
                "feed",
                "abstain",
                request_id,
                "--resolver-id",
                resolver_id,
                "--reason",
                payload,
            ],
        )
    if result.returncode != 0:
        # CAS rejections (someone else moved the chain) are expected.
        print(
            f"[hook_bridge_resolver] {kind} {request_id} non-fatal: "
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
            apply_decision(item, decide(item), args.resolver_id, args.rimz_bin)
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
