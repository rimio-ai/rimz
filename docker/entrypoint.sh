#!/bin/sh
set -eu

if [ "${1:-}" = wrapper ]; then
    exec cat /usr/local/share/rimz/rimz-docker
fi

if [ "$(id -u)" -eq 0 ]; then
    repo_uid="$(stat -c %u .)"
    repo_gid="$(stat -c %g .)"
    if [ "$repo_uid" -ne 0 ] && \
        { [ "$repo_uid" -ne "$(id -u rimz)" ] || [ "$repo_gid" -ne "$(id -g rimz)" ]; }; then
        groupmod -o -g "$repo_gid" rimz
        # Keep usermod from walking a volume that may already have the right owner.
        usermod -d /nonexistent rimz
        usermod -o -u "$repo_uid" -g "$repo_gid" rimz
        usermod -d /home/rimz rimz
    fi
    if [ "$(stat -c %u:%g /home/rimz)" != "$(id -u rimz):$(id -g rimz)" ]; then
        chown -R rimz:rimz /home/rimz
    fi
    exec setpriv --reuid rimz --regid rimz --init-groups \
        env HOME=/home/rimz USER=rimz LOGNAME=rimz "$0" "$@"
fi

if [ "${GIT_AUTHOR_NAME+x}" = x ]; then
    GIT_COMMITTER_NAME="${GIT_COMMITTER_NAME-$GIT_AUTHOR_NAME}"
    export GIT_COMMITTER_NAME
fi
if [ "${GIT_AUTHOR_EMAIL+x}" = x ]; then
    GIT_COMMITTER_EMAIL="${GIT_COMMITTER_EMAIL-$GIT_AUTHOR_EMAIL}"
    export GIT_COMMITTER_EMAIL
fi
if [ -z "${GIT_AUTHOR_NAME:-}" ] && [ -z "$(git config --get user.name || true)" ]; then
    echo 'Git identity missing: pass GIT_AUTHOR_NAME and GIT_AUTHOR_EMAIL, or use rimz-docker.' >&2
fi

for stale in \
    "$HOME/.local/bin/claude" \
    "$HOME/.local/share/claude" \
    "$HOME/.claude/downloads" \
    "$HOME/.local/bin/codex" \
    "$HOME/.opencode/bin/opencode"; do
    if [ -e "$stale" ] || [ -L "$stale" ]; then
        printf 'Removing old image install: %s\n' "$stale" >&2
        rm -rf -- "$stale"
    fi
done

case "${1:-shell}" in
    shell|web)
        for agent in claude codex pi opencode; do
            if ! rimz hooks install "$agent" >/dev/null; then
                printf 'Could not refresh %s hooks; run rimz hooks install %s to retry.\n' "$agent" "$agent" >&2
            fi
        done
        ;;
esac

case "${1:-shell}" in
    shell)
        printf '%s\n' \
            'RimZ is ready: run `rimz` to open the room.' \
            'Launch an agent with `claude`, `codex`, `pi`, or `opencode`.' \
            'Browser mode: run `rimz-docker web` on the host.'
        exec bash -l
        ;;
    web)
        rimz config set web.interface 0.0.0.0
        rimz web open "$PWD" --print --no-resume
        rimz web token show rimz 2>/dev/null || rimz web token list
        sleep infinity &
        sleeper=$!
        trap 'kill "$sleeper" 2>/dev/null || true; wait "$sleeper" 2>/dev/null || true; exit 0' TERM INT
        wait "$sleeper"
        ;;
    *)
        exec "$@"
        ;;
esac
