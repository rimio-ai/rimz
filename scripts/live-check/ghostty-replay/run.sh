#!/bin/sh
# Replay a kitty graphics capture through Ghostty's own parser and image storage.
#
# usage: scripts/live-check/ghostty-replay/run.sh <capture> [ghostty dir]
#
# <capture> is a kitty.raw from kitty-capture.sh. The script clones Ghostty v1.3.1 into
# [ghostty dir] (default $TMPDIR/ghostty-v1.3.1) on first use, adds the replay test, and
# builds it with zig 0.15.2 from PyPI through uvx. The first build takes minutes; a rerun
# takes seconds. RimZ sends q=2, which suppresses Ghostty's error replies, so the patch
# records each command's response before the quiet check drops it. The script writes one
# "command=N action=A result=R" line per kitty command and one "image=ID" line per image
# left in storage to <capture>.results, then prints the SUMMARY line and a count of each
# error result. A clean replay reads errors=0.
set -eu

[ "$#" -ge 1 ] || { sed -n '3,4p' "$0" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
capture=$(realpath "$1")
ghostty=${2:-${TMPDIR:-/tmp}/ghostty-v1.3.1}
results="$capture.results"

if [ ! -d "$ghostty" ]; then
    git -c advice.detachedHead=false clone -q --depth 1 --branch v1.3.1 https://github.com/ghostty-org/ghostty "$ghostty"
fi
if ! grep -q rimz_replay_result "$ghostty/src/terminal/kitty/graphics_exec.zig"; then
    git -C "$ghostty" apply "$here/graphics_exec.patch"
fi
cp "$here/petchunks_replay.zig" "$ghostty/src/terminal/kitty/"

(cd "$ghostty" && PETCHUNKS_INPUT="$capture" PETCHUNKS_OUTPUT="$results" \
    uvx --from ziglang==0.15.2 python -m ziglang build test \
    -Dapp-runtime=none -Doptimize=ReleaseFast -Dtest-filter=petchunks-replay)
grep SUMMARY "$results"
awk -F 'result=' '/result=/ && $2 !~ /^OK/ { n[$2]++ } END { for (e in n) print n[e], e }' "$results"
