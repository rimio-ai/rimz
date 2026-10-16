#!/bin/sh
set -eu

case "$(uname -m)" in
    x86_64) codex_arch=x86_64; claude_platform=linux-x64 ;;
    aarch64) codex_arch=aarch64; claude_platform=linux-arm64 ;;
    *)
        echo "unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

agents_tmp="$(mktemp -d)"
trap 'rm -rf "$agents_tmp"' EXIT

echo "installing latest Claude Code"
claude_url=https://downloads.claude.ai/claude-code-releases
claude_version="$(
    curl --proto '=https' --tlsv1.2 -fsSL \
        --retry 5 --retry-delay 2 --retry-all-errors \
        "$claude_url/latest"
)"
printf '%s\n' "$claude_version" | jq -Re \
    'test("^[0-9]+\\.[0-9]+\\.[0-9]+(-[A-Za-z0-9.-]+)?$")' >/dev/null
curl --proto '=https' --tlsv1.2 -fsSL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "$claude_url/$claude_version/manifest.json" \
    -o "$agents_tmp/manifest.json"
claude_checksum="$(jq -er --arg platform "$claude_platform" \
    '.platforms[$platform].checksum | strings | select(test("^[a-fA-F0-9]{64}$"))' \
    "$agents_tmp/manifest.json")"
curl --proto '=https' --tlsv1.2 -fsSL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "$claude_url/$claude_version/$claude_platform/claude" \
    -o "$agents_tmp/claude"
printf '%s  %s\n' "$claude_checksum" "$agents_tmp/claude" | sha256sum -c -
install -m 0755 "$agents_tmp/claude" /usr/local/bin/claude

echo "installing latest Codex for ${codex_arch}"
curl --proto '=https' --tlsv1.2 -fL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "https://github.com/openai/codex/releases/latest/download/codex-${codex_arch}-unknown-linux-musl.tar.gz" \
    | tar -xz -C "$agents_tmp"
install -m 0755 "$agents_tmp"/codex-* /usr/local/bin/codex

claude --version
codex --version
