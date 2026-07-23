#!/bin/sh
set -eu

case "${1:-shell}" in
    shell)
        printf '%s\n' \
            'RimZ is ready: run `rimz` to open the room.' \
            'Launch an agent with `claude`, `codex`, `pi`, or `opencode`.' \
            'Browser mode: publish port 8200 and run this image with `web`.'
        exec bash -l
        ;;
    web)
        rimz config set web.interface 0.0.0.0
        rimz web open /workspace --print --no-resume
        exec sleep infinity
        ;;
    *)
        exec "$@"
        ;;
esac
