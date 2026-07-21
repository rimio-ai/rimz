#!/bin/sh

set -eu

version=${RIMZ_VERSION:-latest}
install_dir=${RIMZ_INSTALL_DIR:-}

fancy=1
if [ ! -t 2 ] || [ -z "${TERM:-}" ] || [ "${TERM:-}" = dumb ] || [ -n "${NO_COLOR+x}" ]; then
    fancy=0
fi

locale_name=${LC_ALL:-${LC_CTYPE:-${LANG:-}}}
case "$locale_name" in
    *[Uu][Tt][Ff]8*|*[Uu][Tt][Ff]-8*) ;;
    *) fancy=0 ;;
esac

if [ "$fancy" = 1 ] && ! sleep 0.01 2>/dev/null; then
    fancy=0
fi

esc=$(printf '\033')
reset=
row1=
row2=
row3=
row4=
row5=
row6=
c_ok=
c_dim=
c_head=
c_spin=
c_bold=
c_warn=

if [ "$fancy" = 1 ]; then
    reset="${esc}[0m"
    c_ok="${esc}[38;2;158;206;106m"
    c_dim="${esc}[2;38;2;86;95;137m"
    c_head="${esc}[1;97m"
    c_spin="${esc}[38;2;122;162;247m"
    c_bold="${esc}[1m"
    c_warn="${esc}[38;2;224;175;104m"
    case "${COLORTERM:-}" in
        *[Tt][Rr][Uu][Ee][Cc][Oo][Ll][Oo][Rr]*|*24[Bb][Ii][Tt]*)
            row1="${esc}[38;2;122;162;247m"
            row2="${esc}[38;2;135;160;247m"
            row3="${esc}[38;2;148;159;247m"
            row4="${esc}[38;2;161;157;247m"
            row5="${esc}[38;2;174;156;247m"
            row6="${esc}[38;2;187;154;247m"
            ;;
        *)
            row1="${esc}[34m"
            row2="${esc}[34m"
            row3="${esc}[36m"
            row4="${esc}[36m"
            row5="${esc}[35m"
            row6="${esc}[35m"
            c_ok="${esc}[32m"
            c_dim="${esc}[2m"
            c_spin="${esc}[34m"
            c_warn="${esc}[33m"
            ;;
    esac
fi

cursor_hidden=0
tmp_dir=
dl_pid=
banner_drawn=0
ladder_rows=4
brew_action=

say() {
    printf '%s\n' "$*" >&2
}

restore_cursor() {
    if [ "$cursor_hidden" = 1 ]; then
        printf '%s' "${reset}${esc}[?25h" >&2
        cursor_hidden=0
    fi
}

die() {
    restore_cursor
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

cleanup() {
    restore_cursor
    if [ -n "$dl_pid" ]; then
        kill "$dl_pid" 2>/dev/null || true
        wait "$dl_pid" 2>/dev/null || true
    fi
    [ -z "$tmp_dir" ] || rm -rf "$tmp_dir"
}

finish_install() {
    if [ "$fancy" = 1 ]; then
        printf '\n' >&2
    fi

    if ! path_contains "$dest"; then
        if [ "$fancy" = 1 ]; then
            printf '%sAdd RimZ to your PATH:%s\n' "$c_dim" "$reset" >&2
            printf '%s  export PATH="%s:$PATH"%s\n' "$c_dim" "$dest" "$reset" >&2
        else
            say "Add RimZ to your PATH:"
            printf '  export PATH="%s:$PATH"\n' "$dest" >&2
        fi
    fi

    resolved=$(command -v rimz 2>/dev/null || true)
    if [ -n "$resolved" ] && [ "$resolved" != "$installed" ]; then
        if [ "$fancy" = 1 ]; then
            printf '%sNote: %s currently shadows %s%s\n' "$c_dim" "$resolved" "$installed" "$reset" >&2
        else
            say "Note: $resolved currently shadows $installed"
        fi
    fi

    if [ "$fancy" = 1 ]; then
        printf '%sNext: rimz doctor%s\n' "$c_bold" "$reset" >&2
        restore_cursor
    else
        say "Next step: rimz doctor"
    fi
}

banner_line1='  ██████╗ ██╗███╗   ███╗███████╗'
banner_line2='  ██╔══██╗██║████╗ ████║╚══███╔╝'
banner_line3='  ██████╔╝██║██╔████╔██║  ███╔╝'
banner_line4='  ██╔══██╗██║██║╚██╔╝██║ ███╔╝'
banner_line5='  ██║  ██║██║██║ ╚═╝ ██║███████╗'
banner_line6='  ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝'

banner_row() {
    case "$1" in
        1) banner_text=$banner_line1; banner_color=$row1 ;;
        2) banner_text=$banner_line2; banner_color=$row2 ;;
        3) banner_text=$banner_line3; banner_color=$row3 ;;
        4) banner_text=$banner_line4; banner_color=$row4 ;;
        5) banner_text=$banner_line5; banner_color=$row5 ;;
        6) banner_text=$banner_line6; banner_color=$row6 ;;
    esac
}

draw_banner_frame() {
    frame_scan=$1
    frame_row=1
    while [ "$frame_row" -le 6 ]; do
        banner_row "$frame_row"
        if [ "$frame_row" -lt "$frame_scan" ]; then
            printf '\r\033[K%s%s%s\n' "$banner_color" "$banner_text" "$reset" >&2
        elif [ "$frame_row" -eq "$frame_scan" ]; then
            printf '\r\033[K%s%s%s\n' "$c_head" "$banner_text" "$reset" >&2
        else
            printf '\r\033[K\n' >&2
        fi
        frame_row=$((frame_row + 1))
    done
}

draw_banner_flash() {
    flash_row=1
    while [ "$flash_row" -le 6 ]; do
        banner_row "$flash_row"
        printf '\r\033[K%s%s%s\n' "$c_head" "$banner_text" "$reset" >&2
        flash_row=$((flash_row + 1))
    done
}

animate_banner() {
    printf '%s' "${esc}[?25l" >&2
    cursor_hidden=1

    scan_row=1
    while [ "$scan_row" -le 6 ]; do
        [ "$scan_row" -eq 1 ] || printf '%s' "${esc}[6A" >&2
        draw_banner_frame "$scan_row"
        sleep 0.08
        scan_row=$((scan_row + 1))
    done

    printf '%s' "${esc}[6A" >&2
    draw_banner_flash
    sleep 0.08
    printf '%s' "${esc}[6A" >&2
    draw_banner_frame 7
    printf '%s  The control room for your coding agents%s\n\n' "$c_dim" "$reset" >&2
}

print_row() {
    print_glyph_color=$1
    print_glyph=$2
    print_label=$3
    print_detail=$4
    print_text_color=$5
    printf '%s%s%s  %s%-9s%s%s%s' \
        "$print_glyph_color" "$print_glyph" "$reset" \
        "$print_text_color" "$print_label" "$reset" \
        "$print_detail" "$reset" >&2
}

draw_row() {
    draw_offset=$((ladder_rows + 1 - $1))
    shift
    printf '\033[%sA\r\033[K' "$draw_offset" >&2
    print_row "$@"
    printf '\033[%sB\r' "$draw_offset" >&2
}

spinner_glyph() {
    case "$1" in
        0) printf '⠋' ;;
        1) printf '⠙' ;;
        2) printf '⠹' ;;
        3) printf '⠸' ;;
        4) printf '⠼' ;;
        5) printf '⠴' ;;
        6) printf '⠦' ;;
        7) printf '⠧' ;;
        8) printf '⠇' ;;
        9) printf '⠏' ;;
    esac
}

download_size() {
    if [ -f "$tmp_dir/$archive" ]; then
        size_bytes=$(wc -c < "$tmp_dir/$archive")
    else
        size_bytes=0
    fi

    if [ "$size_bytes" -ge 1048576 ]; then
        size_tenths=$((size_bytes * 10 / 1048576))
        printf '%s.%s MB' "$((size_tenths / 10))" "$((size_tenths % 10))"
    elif [ "$size_bytes" -ge 1024 ]; then
        size_tenths=$((size_bytes * 10 / 1024))
        printf '%s.%s KB' "$((size_tenths / 10))" "$((size_tenths % 10))"
    else
        printf '%s B' "$size_bytes"
    fi
}

verify_archive() {
    if [ "$checksum_tool" = sha256sum ]; then
        (cd "$tmp_dir" && sha256sum -c --ignore-missing SHA256SUMS)
    else
        (cd "$tmp_dir" && shasum -a 256 -c --ignore-missing SHA256SUMS)
    fi
}

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

trap cleanup 0
trap 'exit 1' HUP INT TERM
tmp_dir=$(mktemp -d)

use_brew() {
    [ "$system" = Darwin ] || return 1
    [ "$version" = latest ] || return 1
    [ -z "$install_dir" ] || return 1
    [ "${RIMZ_NO_BREW:-0}" != 1 ] || return 1
    command -v brew >/dev/null 2>&1 || return 1

    brew_action=install
    if brew list --versions rimz >/dev/null 2>&1; then
        brew_action=upgrade
    elif command -v rimz >/dev/null 2>&1; then
        return 1
    fi
}

brew_failed() {
    brew_failure=$1
    if [ "$fancy" = 1 ]; then
        draw_row 2 "$c_warn" '✗' homebrew "$brew_failure" ''
        restore_cursor
        cat "$tmp_dir/brew.log" >&2
    fi
    say "Homebrew install failed ($brew_failure); falling back to the release download"
    return 1
}

run_brew() {
    if [ "$brew_action" = upgrade ]; then
        brew update || return
    fi
    brew "$brew_action" rimio-ai/rimz/rimz
}

install_with_brew() {
    ladder_rows=3
    if [ "$fancy" = 1 ]; then
        animate_banner
        banner_drawn=1

        print_row "$c_ok" '✓' platform "$target" ''; printf '\n' >&2
        print_row "$c_spin" '⠋' homebrew "$brew_action rimz" ''; printf '\n' >&2
        print_row "$c_dim" '·' install '' "$c_dim"; printf '\n' >&2

        run_brew > "$tmp_dir/brew.log" 2>&1 &
        dl_pid=$!
        spinner_frame=0
        while kill -0 "$dl_pid" 2>/dev/null; do
            spinner=$(spinner_glyph "$spinner_frame")
            draw_row 2 "$c_spin" "$spinner" homebrew "$brew_action rimz" ''
            spinner_frame=$(((spinner_frame + 1) % 10))
            sleep 0.08
        done
        if wait "$dl_pid"; then
            brew_status=0
        else
            brew_status=$?
        fi
        dl_pid=
        if [ "$brew_status" -ne 0 ]; then
            brew_failed "brew $brew_action exited unsuccessfully"
            return 1
        fi

        draw_row 2 "$c_ok" '✓' homebrew "brew $brew_action complete" ''
        draw_row 3 "$c_spin" '⠋' install 'resolving binary' ''
    else
        say "Installing RimZ with Homebrew"
        if ! run_brew; then
            brew_failed "brew $brew_action exited unsuccessfully"
            return 1
        fi
    fi

    brew_formula_prefix=$(brew --prefix rimz 2>/dev/null || true)
    if [ -n "$brew_formula_prefix" ] && [ -f "$brew_formula_prefix/bin/rimz" ]; then
        installed=$brew_formula_prefix/bin/rimz
    else
        installed=$(command -v rimz 2>/dev/null || true)
    fi
    if [ -z "$installed" ] || [ ! -f "$installed" ]; then
        brew_failed 'installed binary not found'
        return 1
    fi
    dest=${installed%/*}

    # Homebrew exposes formula files under `brew --prefix rimz` but links
    # commands from its global prefix. Report that linked path when present so
    # every user-facing path describes the command users will run.
    brew_prefix=$(brew --prefix 2>/dev/null || true)
    if [ -n "$brew_prefix" ] && [ -f "$brew_prefix/bin/rimz" ]; then
        installed=$brew_prefix/bin/rimz
        dest=$brew_prefix/bin
    fi

    if [ "$fancy" = 1 ]; then
        if installed_version=$("$installed" --version 2>&1); then
            draw_row 3 "$c_ok" '✓' install "$installed  $installed_version" ''
        else
            printf '%s\n' "$installed_version" >> "$tmp_dir/brew.log"
            brew_failed 'version check failed'
            return 1
        fi
    else
        if ! "$installed" --version >&2; then
            brew_failed 'version check failed'
            return 1
        fi
        say "Installed RimZ to $installed"
    fi
    return 0
}

if use_brew && install_with_brew; then
    finish_install
    exit 0
fi

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

archive="rimz-$target.tar.gz"
if [ "$version" = latest ]; then
    base_url=https://github.com/rimio-ai/rimz/releases/latest/download
else
    base_url="https://github.com/rimio-ai/rimz/releases/download/$version"
fi

(
    download_child=
    stop_download() {
        if [ -n "$download_child" ]; then
            kill "$download_child" 2>/dev/null || true
            wait "$download_child" 2>/dev/null || true
        fi
        exit 1
    }
    trap stop_download HUP INT TERM

    download_file() {
        curl -fsSL --proto '=https' --tlsv1.2 -o "$1" "$2" &
        download_child=$!
        if wait "$download_child"; then
            download_result=0
        else
            download_result=$?
        fi
        download_child=
        return "$download_result"
    }

    if download_file "$tmp_dir/$archive" "$base_url/$archive" && \
        download_file "$tmp_dir/SHA256SUMS" "$base_url/SHA256SUMS"; then
        download_status=0
    else
        download_status=$?
    fi
    printf '%s\n' "$download_status" > "$tmp_dir/dl.status"
    exit "$download_status"
) > "$tmp_dir/curl.log" 2>&1 &
dl_pid=$!

if [ "$fancy" = 1 ]; then
    ladder_rows=4
    if [ "$banner_drawn" = 0 ]; then
        animate_banner
    else
        printf '%s' "${esc}[?25l" >&2
        cursor_hidden=1
    fi

    print_row "$c_ok" '✓' platform "$target" ''; printf '\n' >&2
    print_row "$c_spin" '⠋' download "rimz $version" ''; printf '\n' >&2
    print_row "$c_dim" '·' verify '' "$c_dim"; printf '\n' >&2
    print_row "$c_dim" '·' install '' "$c_dim"; printf '\n' >&2

    spinner_frame=0
    while kill -0 "$dl_pid" 2>/dev/null; do
        spinner=$(spinner_glyph "$spinner_frame")
        current_size=$(download_size)
        draw_row 2 "$c_spin" "$spinner" download "rimz $version · $current_size" ''
        spinner_frame=$(((spinner_frame + 1) % 10))
        sleep 0.08
    done
else
    say "Downloading RimZ $version for $target"
fi

if wait "$dl_pid"; then
    dl_wait_status=0
else
    dl_wait_status=$?
fi
dl_pid=

dl_status=$dl_wait_status
if [ -r "$tmp_dir/dl.status" ]; then
    IFS= read -r dl_status < "$tmp_dir/dl.status" || dl_status=$dl_wait_status
fi

if [ "$dl_status" -ne 0 ]; then
    restore_cursor
    cat "$tmp_dir/curl.log" >&2
    if [ "$fancy" = 1 ]; then
        die "download failed; see output above"
    fi
    exit "$dl_status"
fi

if [ "$fancy" = 1 ]; then
    current_size=$(download_size)
    draw_row 2 "$c_ok" '✓' download "rimz $version · $current_size" ''
    draw_row 3 "$c_spin" '⠋' verify "$archive" ''
else
    say "Verifying $archive"
fi

if [ "$fancy" = 1 ]; then
    if verify_archive > "$tmp_dir/verify.log" 2>&1; then
        draw_row 3 "$c_ok" '✓' verify 'sha256 ok' ''
    else
        restore_cursor
        cat "$tmp_dir/verify.log" >&2
        die "checksum verification failed for $archive"
    fi
else
    verify_archive >&2
fi

if [ "$fancy" = 1 ]; then
    draw_row 4 "$c_spin" '⠋' install "${install_dir:-selecting destination}" ''
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

if [ "$fancy" = 1 ]; then
    if installed_version=$("$installed" --version 2>&1); then
        draw_row 4 "$c_ok" '✓' install "$installed  $installed_version" ''
    else
        version_status=$?
        restore_cursor
        printf '%s\n' "$installed_version" >&2
        exit "$version_status"
    fi
else
    "$installed" --version >&2
    say "Installed RimZ to $installed"
fi

finish_install
