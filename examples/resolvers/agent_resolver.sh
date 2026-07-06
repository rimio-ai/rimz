#!/bin/sh
# Example notification handler that delegates a waiting ask to another agent.
#
# Wire with:
#   [[notifications.handler]]
#   when = { kind = ["waiting"] }
#   command = "examples/resolvers/agent_resolver.sh"

set -eu

rimz=${RIMZ_BIN:-rimz}
request_id=${RIMZ_NOTIFY_REQUEST_ID:-}
pane=${RIMZ_NOTIFY_PANE:-}
root=${RIMZ_NOTIFY_ROOT:-}
kind=${RIMZ_RESOLVER_AGENT_KIND:-codex}

if [ -z "$request_id" ]; then
  exit 0
fi

brief=$("$rimz" feed show "$request_id" --json 2>/dev/null || true)
if [ -z "$brief" ]; then
  exit 0
fi

prompt="Inspect feed item $request_id. If bounded policy applies, answer in pane $pane under $root using rimz pane send, then run: rimz feed resolve $request_id --method pane-send --by agent-resolver --decision '{\"choice\":\"sent\"}'. If unsure, do nothing.

$brief"

exec "$rimz" agents "$kind" -p "$prompt"
