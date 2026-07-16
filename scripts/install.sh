#!/bin/sh

set -eu

version=${RIMZ_VERSION:-latest}
install_dir=${RIMZ_INSTALL_DIR:-}

say() {
    printf '%s\n' "$*" >&2
}

die() {
    say "rimz install: $*"
    exit 1
}

require() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required; install it and retry"
}

path_contains() {
    remaining=${PATH:-}:
    while [ -n "$remaining" ]; do
        entry=${remaining%%:*}
        [ "$entry" = "$1" ] && return 0
        remaining=${remaining#*:}
    done
    return 1
}

require curl
require tar
require install

if command -v sha256sum >/dev/null 2>&1; then
    checksum_tool=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    checksum_tool=shasum
else
    die "sha256sum or shasum is required; install coreutils and retry"
fi

system=$(uname -s)
machine=$(uname -m)

case "$system:$machine" in
    Linux:x86_64)
        ldd_version=$(ldd --version 2>&1 || true)
        case "$ldd_version" in
            *musl*|*Musl*|*MUSL*)
                die "prebuilt Linux binaries require glibc; install with: cargo install --locked rimz"
                ;;
        esac
        target=x86_64-unknown-linux-gnu
        ;;
    Linux:aarch64|Linux:arm*)
        die "prebuilt ARM Linux binaries are unavailable; install with: cargo install --locked rimz"
        ;;
    Darwin:arm64)
        target=aarch64-apple-darwin
        ;;
    Darwin:x86_64)
        target=x86_64-apple-darwin
        ;;
    *)
        die "unsupported platform $system/$machine; see https://github.com/rimio-ai/rimz/blob/main/docs/guide/installation.md"
        ;;
esac

archive="rimz-$target.tar.gz"
if [ "$version" = latest ]; then
    base_url=https://github.com/rimio-ai/rimz/releases/latest/download
else
    base_url="https://github.com/rimio-ai/rimz/releases/download/$version"
fi

tmp_dir=
cleanup() {
    [ -z "$tmp_dir" ] || rm -rf "$tmp_dir"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
tmp_dir=$(mktemp -d)

say "Downloading RimZ $version for $target"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp_dir/$archive" "$base_url/$archive"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp_dir/SHA256SUMS" "$base_url/SHA256SUMS"

say "Verifying $archive"
if [ "$checksum_tool" = sha256sum ]; then
    (cd "$tmp_dir" && sha256sum -c --ignore-missing SHA256SUMS) >&2
else
    (cd "$tmp_dir" && shasum -a 256 -c --ignore-missing SHA256SUMS) >&2
fi

tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"
binary="$tmp_dir/rimz-$target/rimz"
[ -f "$binary" ] || die "$archive does not contain rimz-$target/rimz"

if [ -n "$install_dir" ]; then
    dest=$install_dir
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
    dest=/usr/local/bin
else
    [ -n "${HOME:-}" ] || die "HOME is not set; set RIMZ_INSTALL_DIR and retry"
    dest=$HOME/.local/bin
fi

mkdir -p "$dest" || die "cannot create install directory $dest"
[ -d "$dest" ] || die "install destination is not a directory: $dest"
[ -w "$dest" ] || die "install destination is not writable: $dest"
dest=$(CDPATH= cd "$dest" && pwd -P) || die "cannot resolve install directory $dest"

installed=$dest/rimz
install -m 0755 "$binary" "$installed" || die "failed to install RimZ to $installed"

"$installed" --version >&2
say "Installed RimZ to $installed"

if ! path_contains "$dest"; then
    say "Add RimZ to your PATH:"
    printf '  export PATH="%s:$PATH"\n' "$dest" >&2
fi

resolved=$(command -v rimz 2>/dev/null || true)
if [ -n "$resolved" ] && [ "$resolved" != "$installed" ]; then
    say "Note: $resolved currently shadows $installed"
fi

say "Next step: rimz doctor"
