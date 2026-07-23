#!/bin/sh
set -eu

case "$(uname -m)" in
    x86_64) codex_arch=x86_64 ;;
    aarch64) codex_arch=aarch64 ;;
    *)
        echo "unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

mkdir -p "$HOME/.local/bin"

echo "installing latest Claude Code"
claude_installer="$(mktemp)"
curl --proto '=https' --tlsv1.2 -fsSL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    https://claude.ai/install.sh \
    -o "$claude_installer"
bash "$claude_installer"
rm -f "$claude_installer"

echo "installing latest Codex for ${codex_arch}"
codex_tmp="$(mktemp -d)"
curl --proto '=https' --tlsv1.2 -fL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "https://github.com/openai/codex/releases/latest/download/codex-${codex_arch}-unknown-linux-musl.tar.gz" \
    | tar -xz -C "$codex_tmp"
install -m 0755 "$codex_tmp"/codex-* "$HOME/.local/bin/codex"
rm -rf "$codex_tmp"

echo "installing latest OpenCode"
opencode_installer="$(mktemp)"
curl --proto '=https' --tlsv1.2 -fsSL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    https://opencode.ai/install \
    -o "$opencode_installer"
bash "$opencode_installer" --no-modify-path
rm -f "$opencode_installer"

claude --version
codex --version
opencode --version
