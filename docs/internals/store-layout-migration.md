# Moving this machine to room layout 2

Run this after merge, from a host shell outside every room being stopped. There is no automatic layout migration: the new binary refuses old rooms, and cross-room scans retain and report them. `workspace migrate` changes project identity, not layout. Prefer reset unless the room's history matters. The [class catalog](./store.md#what-is-on-disk) is the destination authority.

## Find old rooms

These commands target this Linux machine. Keep the same shell for the variables below; `RIMZ_HOME` and the runtime fallback match path resolution.

```sh
migration_home="${RIMZ_HOME:-$HOME/.rimz}"
migration_runtime="${XDG_RUNTIME_DIR:-/tmp/rimz-$(id -u)}/rimz/ws"
for migration_record in "$migration_home"/ws/*/workspace.json; do
    test -f "$migration_record" || continue
    jq -r 'select((.layout // 1) != 2) | [input_filename, .project_root, .session_name] | @tsv' "$migration_record"
done
if test -d "$migration_runtime"; then
    ls -d -- "$migration_runtime"/*
fi
```

All room runtime trees are disposable after their processes stop, including `tmp*-*` orphan directories. Do not remove the sibling machine-shared runtime directory.

## Stop, then reset (recommended)

Before replacing the installed binary, stop each team's live cohort with `rimz teams stop <team#channel>` in its project. There is no top-level `rimz stop`: use the old room executable's `reset --yes --no-start` for each old room. This closes the mux session, sweeps its processes, cancels runs and archives the active log. The old reset also removes diagnostic captures, room tmp and rewritten skill copies; copy anything you intend to preserve before that boundary. Do not launch another room until migration finishes.

Choose the old room from the listing, then run:

```sh
printf 'Old room directory name: '
read -r migration_name
case "$migration_name" in ''|*/*|.|..) exit 1 ;; esac
migration_room="$migration_home/ws/$migration_name"
jq -e '(.layout // 1) == 1' "$migration_room/workspace.json" || exit 1
migration_project=$(jq -er '.project_root' "$migration_room/workspace.json")
"$migration_room/rimz" reset --yes --no-start "$migration_project"
```

Repeat for every old room. If a recorded project no longer exists, stop its recorded mux session with the owning backend and terminate its remaining agent/watch processes before touching its files. For a move, continue to the next section instead of deleting state. For a reset, remove each selected stopped room:

```sh
rm -rf -- "$migration_room"
```

Once **all** rooms, including any layout-2 rooms, are stopped, remove the runtime room trees:

```sh
rm -rf -- "$migration_runtime"
```

This loses room transcripts, message history and queue, run records, scratch, rewritten skill copies, room budget choices, and event history. It does not touch project files, config or definitions, staged builds under `builds/`, provider caches under `cache/providers/`, or provider login homes under `accounts/`. Provider caches are not login credentials. Install the merged binary and run `rimz start` in each project to create layout 2.

## Optional move of one stopped room

Use this instead of deleting the selected state directory. Old names below come from the pre-layout path constructors. The destination must not already contain classed state; do not run the block twice after a partial move without inspecting it. Room identity (`workspace.json` and `rimz`) stays at the root. Live runtime state is not carried over, except optional binding diagnostics and standing fleet choices.

```sh
(
    set -eu
    cd "$migration_room"
    jq -e '(.layout // 1) == 1' workspace.json
    rm -rf -- locks
    mkdir -p log records/messages audit/messages cache owned tmp locks
    migration_move() {
        if test -e "$1"; then mv -- "$1" "$2"; fi
    }
    migration_move events.log.jsonl log/events.log.jsonl
    migration_move events.log.archive log/archive
    migration_move agents.carryover.json records/agents-carryover.json
    for migration_file in channels.json boot.json last-death.json loop-instances.json live-roster.json; do
        migration_move "$migration_file" "records/$migration_file"
    done
    migration_move messages/messages.jsonl records/messages/messages.jsonl
    migration_move transcript audit/transcript
    migration_move crashes audit/crashes
    migration_move diag-frames audit/diag-frames
    for migration_file in diag*.jsonl notify*.jsonl plugin-presence*.jsonl; do
        migration_move "$migration_file" "audit/$migration_file"
    done
    migration_move runs owned/runs
    if test -f messages/history.jsonl; then
        migration_seconds=$(stat -c '%Y' messages/history.jsonl)
        migration_start=$((migration_seconds / 604800 * 604800))
        migration_bucket=$(date -u -d "@$migration_start" '+%Y-%m-%d.jsonl')
        mv -- messages/history.jsonl "audit/messages/$migration_bucket"
    fi
    migration_old_runtime="$migration_runtime/$migration_name"
    for migration_file in "$migration_old_runtime"/binding*.jsonl; do
        if test -f "$migration_file"; then mv -- "$migration_file" audit/; fi
    done
    if test -f "$migration_old_runtime/budget.fleet.json"; then
        jq '{override_spec, raised_cap_usd, disabled} | with_entries(select(.value != null))' \
            "$migration_old_runtime/budget.fleet.json" > records/budget.fleet.json
    fi
    rm -rf -- snapshots auto-gc.json doctor-cleared.json skills tmp messages
    jq '.layout = 2' workspace.json > workspace.json.layout-tmp
    mv -- workspace.json.layout-tmp workspace.json
)
```

The old `locks/*.stamp` files are debounce caches, not flock files. Discard them with old locks; writers recreate stamps under `cache/` and every room flock under state `locks/`. The move recreates that directory after discarding the old contents. Old `tmp/agents/<handle>/` scratch and room-wide `skills/` copies are discarded, not moved into owned units. `live-roster.json` is preserved under records because rebirth cannot rebuild it. If the old reset already removed files, the optional moves skip them. In particular, its runtime cleanup loses the old fleet choices and binding log: restore saved copies at their old paths before running the move, or reapply the fleet cap with `rimz budget` afterward.

The history filename rule is the UTC date at `floor(unix_seconds / 604800) * 604800`, followed by `.jsonl`, not the file's arbitrary calendar day. The block uses legacy history mtime (positive Unix time on this machine); `1970-01-01.jsonl` and `1970-01-08.jsonl` are the first two windows. New writers use each message's `updated_at`. Moving preserves mtime, so the audit sweep can immediately remove old history under the 30-day/64-MiB rule. Transcript buckets already use this naming rule. No framed log payloads or message records are rewritten.

After every room is stopped and moved or reset, delete the entire old runtime room trees as above, including any runtime `locks/`; no room flock remains there. Install the merged binary before reopening: an old binary must not write into a layout-2 room.

## Verify

For each selected project, use the newly installed binary:

```sh
cd "$migration_project"
rimz paths --json
rimz gc --dry-run --json
rimz start
```

`paths` keeps schema `rimz.paths.v1`; its host scratch points into `owned/agents/<handle>/scratch/` for a handled agent and `tmp/scratchpad/` otherwise. After detaching, check the reopened room and a retained agent:

```sh
rimz gc --dry-run --json
printf 'Retained agent handle (including @): '
read -r migration_agent
rimz agents history "$migration_agent"
```

The GC report has per-room `rooms[].classes[]` rows; no room should report incompatible layout. `agents history` reads provider turn history; use `rimz message list` to inspect retained message history as well. A fresh reset has no retained agents to inspect.

## Safe to delete afterwards

Layout-2 handleless rewritten skills live in `cache/skills/<sha256>/`, outside room tmp. They are rebuildable, cleared by reset, and not age-swept. Handled copies remain under `owned/agents/<handle>/skills/`.

With no room processes left, runtime `ws/tmp*-*` orphans can be deleted. Inspect state directories without `workspace.json` before deleting: absence alone does not prove they lack history, and GC deliberately retains unreadable stores that have it. Project files, provider homes, config and staged builds are outside this cleanup. This committed guide is only a migration aid; removing a local copy after verification changes no runtime state.
