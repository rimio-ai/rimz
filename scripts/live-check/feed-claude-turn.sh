#!/bin/sh
# Feed one Claude session's hooks to a held sandbox room, as one of its agents.
#
# usage: scripts/live-check/feed-claude-turn.sh <root> <@handle> [step...]
#
# <root> is the room card's "Sandbox root:" and <@handle> a handle from a "Role:" line
# (@coder#probe). Run it from the checkout root. Steps run in the order given, and the
# default is "start prompt meter stop", one finished turn:
#
#   start      SessionStart
#   prompt     UserPromptSubmit
#   meter      a statusline with a context reading (40,000 prompt tokens)
#   window     a statusline with the window size and no reading
#   compact=N  a finished compaction that leaves N tokens: PreCompact, PostCompact, then
#              SessionStart with source "compact", over a transcript ending in a
#              compact_boundary line
#   stop       Stop
#
# SESSION names the session (default: a new UUID). WINDOW is the context window size
# (default 200000). XTASK is the xtask binary (default target/debug/xtask). The script
# prints the session id and discards each hook's reply. The room applies each hook
# asynchronously, so poll the read that depends on it.
set -eu

[ "$#" -ge 2 ] || { sed -n '3,4p' "$0" >&2; exit 2; }
root=$1 handle=$2
shift 2
[ "$#" -gt 0 ] || set -- start prompt meter stop

xtask=${XTASK:-target/debug/xtask}
session=${SESSION:-$(cat /proc/sys/kernel/random/uuid)}
window=${WINDOW:-200000}
mux=$(jq -r .mux "$root/room.json")
bin=$(jq -r .binary "$root/room.json")
cwd=$(jq -r .worktree "$root/room.json")
echo "session $session"

as_agent() {
    "$xtask" sandbox in "$root" --as "$handle" -- "$bin" "--$mux" "$@" > /dev/null
}

hook() {
    jq -nc --arg s "$session" --arg w "$cwd" "$1" | as_agent hooks feed --source claude
}

statusline() {
    jq -nc --arg s "$session" --argjson n "$window" "$1" | as_agent statusline feed --source claude
}

for step in "$@"; do
    case $step in
    start)
        hook '{hook_event_name:"SessionStart",session_id:$s,cwd:$w,source:"startup"}' ;;
    prompt)
        hook '{hook_event_name:"UserPromptSubmit",session_id:$s,cwd:$w,prompt:"live check"}' ;;
    meter)
        statusline '{session_id:$s,model:{id:"claude-sonnet-4-6",display_name:"Sonnet 4.6"},
            context_window:{context_window_size:$n,used_percentage:(40000*100/$n),
            current_usage:{input_tokens:10000,cache_read_input_tokens:30000,
            cache_creation_input_tokens:0,output_tokens:100}},cost:{total_cost_usd:0.01}}' ;;
    window)
        statusline '{session_id:$s,model:{id:"claude-sonnet-4-6",display_name:"Sonnet 4.6"},
            context_window:{context_window_size:$n,used_percentage:0},cost:{total_cost_usd:0.01}}' ;;
    compact=*)
        transcript="$root/tmp/compacted-$session.jsonl"
        jq -nc --argjson post "${step#compact=}" '{type:"system",subtype:"compact_boundary",
            compactMetadata:{trigger:"manual",preTokens:90000,postTokens:$post}}' > "$transcript"
        for event in PreCompact PostCompact; do
            jq -nc --arg e "$event" --arg s "$session" --arg w "$cwd" --arg t "$transcript" \
                '{hook_event_name:$e,session_id:$s,cwd:$w,transcript_path:$t,trigger:"manual"}' |
                as_agent hooks feed --source claude
        done
        jq -nc --arg s "$session" --arg w "$cwd" --arg t "$transcript" \
            '{hook_event_name:"SessionStart",session_id:$s,cwd:$w,transcript_path:$t,source:"compact"}' |
            as_agent hooks feed --source claude ;;
    stop)
        hook '{hook_event_name:"Stop",session_id:$s,cwd:$w,last_assistant_message:"done"}' ;;
    *)
        echo "unknown step: $step" >&2
        exit 2 ;;
    esac
done
