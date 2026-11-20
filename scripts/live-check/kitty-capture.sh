#!/bin/sh
# Record the kitty graphics a held tmux room's sidebars send, and tally them.
#
# usage: scripts/live-check/kitty-capture.sh <root> [seconds]
#
# <root> is the room card's "Sandbox root:". Run it from the checkout root. The room's own
# client reports tmux-256color, so no sidebar sends pixels to it. The script detaches that
# client and attaches one recorded by script(1) that reports xterm-kitty, for <seconds>
# (default 30, which covers the sidebar's 10-second capability refresh). It leaves the bytes
# in <root>/tmp/kitty.raw and prints two tallies: each distinct graphics command with ids
# and positions masked, then the number of transmits per image id. The room's server is
# private to the room, so every terminal client on it is the room's own. None is
# reattached: afterwards the room has no client.
set -eu

[ "$#" -ge 1 ] || { sed -n '3,4p' "$0" >&2; exit 2; }
root=$1 seconds=${2:-30}
xtask=${XTASK:-target/debug/xtask}
[ "$(jq -r .mux "$root/room.json")" = tmux ] || { echo "kitty capture is tmux-only" >&2; exit 2; }
session=$(jq -r .session "$root/room.json")
socket="$root/runtime/rimz/tmux/server"
raw="$root/tmp/kitty.raw"

t() { "$xtask" sandbox in "$root" -- tmux -S "$socket" "$@"; }

# The machine rarely has xterm-kitty terminfo, so alias it from xterm-256color.
mkdir -p "$root/tmp/terminfo"
infocmp -x xterm-256color | sed '1,/^xterm-256color|/s/^xterm-256color|/xterm-kitty|/' |
    tic -x -o "$root/tmp/terminfo" -

# Control-mode clients (flag 1) are RimZ's own link to the server: leave them attached.
for client in $(t list-clients -F '#{client_control_mode} #{client_name}' | awk '$1==0{print $2}'); do
    t detach-client -t "$client"
done

# xtask sandbox in turns timeout's expected exit 124 into exit 1, so judge the recording by
# its file instead.
rm -f "$raw"
"$xtask" sandbox in "$root" -- sh -c "TERMINFO='$root/tmp/terminfo' TERM=xterm-kitty \
    timeout $seconds script -qfec 'tmux -S $socket attach -t $session' '$raw'" \
    < /dev/null > /dev/null 2>&1 || true
[ -s "$raw" ] || { echo "no recording at $raw" >&2; exit 1; }

echo "== graphics commands"
grep -ao "$(printf '\033')_G[^;$(printf '\033')]*" "$raw" | sed -E 's/i=[0-9]+/i=N/; s/[xy]=[0-9]+//g' |
    sort | uniq -c | sort -rn
echo "== transmits per image id"
grep -ao "$(printf '\033')_Ga=t[^;$(printf '\033')]*" "$raw" | grep -o 'i=[0-9]*' | sort | uniq -c
