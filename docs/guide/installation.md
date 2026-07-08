# Installation

Rimz is a single binary for macOS and Linux. Pick one install path: [Homebrew](#install-with-homebrew-macos) on macOS, a [prebuilt binary](#install-a-prebuilt-binary) from the releases page, or [Cargo](#install-with-cargo). [Building from source](#build-from-source) is for working on Rimz itself or for platforms the prebuilt paths miss.

## Prerequisites

- **macOS or Linux.**
- **A terminal multiplexer** — Zellij 0.44 or newer, or tmux 3.5 or newer. One is enough; both are first-class. Distribution packages are often too old; [get a current Zellij or tmux](#get-a-current-zellij-or-tmux) has install recipes for current builds.
- **The agent CLIs you plan to run** — Claude Code, Codex, Pi, or OpenCode, installed per their own docs. Rimz drives the stock CLIs and bundles none of them.
- **Git** — agent worktrees and the sidebar's git status use it.

Rimz refuses to start against a multiplexer below the minimum supported version, and `rimz doctor` reports the installed version and whether it clears that floor. On tmux, 3.6 or newer gives the best experience ([why](#rimz-doctor-flags-the-multiplexer-as-unsupported)).

The multiplexer needs no configuration for Rimz: every room sets its own options on session start and reattach, and your existing Zellij or tmux config keeps owning your theme, shell, and keybinds. The room's essential settings are in [set up your machine](./setup.md#configure-your-multiplexer), and a full baseline for your own sessions is [Zellij and tmux baselines](./multiplexer.md).

## Install with Homebrew (macOS)

```sh
brew tap rimio/homebrew-rimz
brew install rimz
```

`brew upgrade rimz` picks up new releases.

## Install a prebuilt binary

Every [release](https://github.com/rimio-ai/rimz/releases) ships one archive per platform plus a `SHA256SUMS` file:

| Archive | Platform |
| --- | --- |
| `rimz-x86_64-unknown-linux-gnu.tar.gz` | Linux, x86_64 |
| `rimz-aarch64-apple-darwin.tar.gz` | macOS, Apple silicon |
| `rimz-x86_64-apple-darwin.tar.gz` | macOS, Intel |

Download, verify, and install (shown for Linux; swap the archive name on macOS and verify with `shasum -a 256 -c --ignore-missing`):

```sh
curl -fsSLO https://github.com/rimio-ai/rimz/releases/latest/download/rimz-x86_64-unknown-linux-gnu.tar.gz
curl -fsSLO https://github.com/rimio-ai/rimz/releases/latest/download/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf rimz-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 rimz-x86_64-unknown-linux-gnu/rimz /usr/local/bin/rimz
```

Three notes on the archives:

- The Linux binary targets x86_64 and links against a current glibc. On an ARM Linux machine, or if the binary reports a `GLIBC` version error, use the [Cargo install](#install-with-cargo) instead.
- The macOS binaries carry an ad-hoc signature and install cleanly over `curl`. A browser download gets quarantined by Gatekeeper; `xattr -d com.apple.quarantine rimz` clears it.
- The `latest-main` prerelease carries a rolling build of the default branch, for when you want a fix that has not been tagged yet.

## Install with Cargo

```sh
cargo install --locked rimz
```

This builds Rimz from crates.io and works on any supported platform, including ARM Linux. It needs the Rust toolchain (install through [rustup](https://rustup.rs)) and a C linker (`build-essential` on Debian/Ubuntu, `xcode-select --install` on macOS). The crate ships Rimz's Zellij plugin as a prebuilt WebAssembly artifact, so no extra Rust targets are involved.

## Verify the install

```sh
rimz --version
rimz doctor
```

`rimz doctor` reports the multiplexer it selected, its version and whether it clears the floor, hook status, and room health — the fastest read on whether a fresh machine is ready. From here, walk [the quickstart](./quickstart.md), then make the machine comfortable with [set up your machine](./setup.md).

## Get a current Zellij or tmux

A distribution's packaged tmux is usually behind (Debian 12 ships 3.3a, Debian 13 ships 3.5a), and most distributions do not package Zellij at all. When `tmux -V` or `zellij --version` reads below the floor, install a current build.

On macOS, Homebrew's builds of both are current:

```sh
brew install zellij
brew install tmux         # one is enough; keep both to choose per project
```

On Linux, Zellij ships a prebuilt binary on its releases page; drop it into your `PATH` (swap `x86_64` for `aarch64` on ARM):

```sh
curl -L https://github.com/zellij-org/zellij/releases/latest/download/zellij-x86_64-unknown-linux-musl.tar.gz \
  | sudo tar -xz -C /usr/local/bin zellij
zellij --version
```

For tmux, the distribution package works when `tmux -V` clears the version you want:

```sh
sudo apt install tmux     # Debian/Ubuntu; dnf and pacman ship it too
```

tmux publishes no prebuilt binaries, so when the packaged one is too old, install a current one through Homebrew on Linux or build the release tarball (the first line installs the build deps):

```sh
sudo apt install -y build-essential pkg-config libevent-dev libncurses-dev bison
curl -fsSLO https://github.com/tmux/tmux/releases/download/3.7/tmux-3.7.tar.gz
tar -xzf tmux-3.7.tar.gz && cd tmux-3.7
./configure && make -j"$(nproc)" && sudo make install
```

## Build from source

The source build is for contributing to Rimz or installing on a platform the prebuilt paths miss. Beyond the [prerequisites](#prerequisites) above it needs a C toolchain, Git, and Rust through `rustup`:

```sh
# Linux — build tools (Debian/Ubuntu shown; use dnf or pacman equivalents)
sudo apt install -y build-essential pkg-config git

# macOS — Apple's command-line tools
xcode-select --install

# Both — Rust through rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Then clone and install:

```sh
git clone https://github.com/rimio-ai/rimz.git
cd rimz
cargo xtask install
```

`cargo xtask install` builds the Zellij presence plugin (a WebAssembly artifact embedded into the binary), builds `rimz`, copies it into `~/.cargo/bin`, and prints the installed version and path. The repo's [rust-toolchain.toml](../../rust-toolchain.toml) pins the toolchain channel, components, and the `wasm32-wasip1` target, and `rustup` applies it on the first build — there is no manual target setup.

To work on Rimz itself (debug builds, tests, the gate stack), continue with [the Rust conventions](../contributing/rust-conventions.md).

## Uninstall

Run `rimz uninstall --all` from outside a Rimz room. It removes installed hooks, live rooms, runtime state, durable stores, per-machine config, and the installed binary; project-local `.rimz/` dirs and Rimz-owned worktrees stay in place for manual review. If you installed through Homebrew, also run `brew uninstall rimz`. Flags for partial removal are in the [maintenance reference](../reference/cli/maintenance.md#reload-reset-gc-and-uninstall).

## Troubleshooting

### `rimz doctor` flags the multiplexer as unsupported

Rimz refuses to start against a multiplexer too old to carry the room options it sets: tmux below 3.5, or Zellij below 0.44. Check the installed version, then [install a current build](#get-a-current-zellij-or-tmux).

```sh
tmux -V
zellij --version
rimz doctor
```

One tmux nuance: on tmux 3.5.x the extended keys Rimz enables (so Shift+Enter and Alt+Enter reach agents as soft newlines) corrupt pasted multiline text; tmux 3.6 fixes the paste too.

### `GLIBC_x.xx not found` running the prebuilt Linux binary

The release binary links against a current glibc, and an older distribution cannot load it. Install through [Cargo](#install-with-cargo) instead, which builds against your system's glibc.

### `can't find crate for core` during a source build

This error during `cargo xtask install` means the compiler Cargo used cannot see the Rust standard library for `wasm32-wasip1`:

```text
error[E0463]: can't find crate for `core`
  = note: the `wasm32-wasip1` target may not be installed
```

The usual cause is a non-rustup Rust shadowing rustup's on your `PATH` — on macOS, Homebrew's `rust` formula is the common culprit. Check where the tools resolve:

```sh
command -v cargo
command -v rustc
rustup show active-toolchain
```

A healthy setup resolves both under `$HOME/.cargo/bin` (or rustup's own bin directory). If they point elsewhere, put rustup first and remove the shadowing toolchain, then rerun `cargo xtask install`:

```sh
brew unlink rust        # macOS, if Homebrew's rust formula is installed
hash -r
```
