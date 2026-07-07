# Installation

Rimz builds from source into one binary that runs inside the Zellij or tmux you already use. The build needs Git, a C linker, a terminal multiplexer, and the Rust toolchain — the steps below install each on Linux and macOS, then verify and tune the result.

The source install also builds a small Zellij plugin that ships embedded in the binary. The plugin compiles to WebAssembly, so the build needs Rust's `wasm32-wasip1` target — and the repo installs it for you: [rust-toolchain.toml](../../rust-toolchain.toml) pins the stable channel, the components, and that target, and `rustup` applies the file the first time you build in the repo. There is no manual target setup.

`cargo install --locked rimz` installs the binary-only crate from crates.io with the presence plugin embedded from a vendored WebAssembly artifact. Zellij pane discovery uses that plugin's topology channel, so Zellij rooms require Zellij 0.44 or newer and a loadable presence plugin; `cargo xtask install` from a source checkout builds and embeds a fresh plugin artifact.

## Prerequisites

Rimz needs four things on your machine.

- **A terminal multiplexer** — Zellij or tmux. Both are first-class; install one, or both and choose per project. Rimz refuses to start against a build too old to carry the room options it sets, so mind the floors: **tmux 3.5 or newer** and **Zellij 0.44 or newer** (tmux 3.5 adds CSI-u soft-newline keys with a multiline-paste caveat; tmux 3.6 preserves paste too; Zellij 0.44 carries the sidebar's single-click jumps and the presence-plugin topology channel). `rimz doctor` reports the installed version and whether it clears the floor.
- **A C linker** — `cc` and `ld` link the final binary. The build pulls in no C libraries of its own; the linker is all the system toolchain provides.
- **Git** — to clone the source.
- **Rust, through `rustup`** — the compiler and Cargo. `rustup` reads the repo's pinned toolchain and installs the matching channel, components, and WebAssembly target on first build.

One thing to know before you reach for a package manager: a distribution's packaged tmux is often older than 3.5 (Debian 12 ships 3.3a), and most distributions do not package Zellij at all. The per-OS steps give a current build of each.

## Linux

Install the build tools and Git.

```sh
# Debian or Ubuntu
sudo apt update
sudo apt install -y build-essential pkg-config git

# Fedora
sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config git

# Arch
sudo pacman -S --needed base-devel pkgconf git
```

Install at least one multiplexer. tmux ships in every distribution; confirm it clears the floor with `tmux -V`, and upgrade from backports, Homebrew on Linux, or source if it predates 3.5.

```sh
# Debian or Ubuntu
sudo apt install -y tmux

# Fedora
sudo dnf install -y tmux

# Arch
sudo pacman -S --needed tmux
```

Zellij is rarely packaged, so install its release binary into your `PATH` (swap `x86_64` for `aarch64` on ARM). Once Rust is installed below, `cargo install zellij` is an alternative that builds it from source.

```sh
curl -L https://github.com/zellij-org/zellij/releases/latest/download/zellij-x86_64-unknown-linux-musl.tar.gz \
  | sudo tar -xz -C /usr/local/bin zellij
zellij --version
```

Install Rust through `rustup`. The installer sets a default stable toolchain and adds `~/.cargo/bin` to your `PATH`.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Clone and install Rimz. `cargo xtask install` builds the presence plugin and the `rimz` binary, copies `rimz` into `~/.cargo/bin`, and prints the installed version and path.

```sh
git clone https://github.com/rimio/rimz.git
cd rimz
cargo xtask install
```

## macOS

Install Apple's command-line tools and a multiplexer. Homebrew's tmux clears the 3.5 floor.

```sh
xcode-select --install
brew install git tmux
brew install zellij        # optional second backend
```

Install `rustup` through Homebrew, which keeps rustup itself current while rustup owns the Rust toolchains.

```sh
brew install rustup
```

Put Homebrew rustup's proxy directory ahead of Homebrew's general bin directory, then install a default toolchain.

```sh
RUSTUP_BIN="$(brew --prefix rustup)/bin"
grep -qxF "export PATH=\"$RUSTUP_BIN:\$PATH\"" ~/.zshrc || \
  echo "export PATH=\"$RUSTUP_BIN:\$PATH\"" >> ~/.zshrc
exec zsh -l
rustup default stable
```

`rustup default stable` installs the toolchain; the repo's pinned components and `wasm32-wasip1` target install themselves the first time you build, so there is nothing else to add by hand.

Homebrew's `rust` formula ships its own `cargo` and `rustc`. Keep them off the path for Rimz builds — `rustup` provisions rustup toolchains, not Homebrew's compiler.

```sh
brew unlink rust || true
hash -r 2>/dev/null || rehash 2>/dev/null || true
```

Confirm Cargo and rustc resolve through rustup, then clone and install.

```sh
command -v cargo
command -v rustc
rustup show active-toolchain

git clone https://github.com/rimio/rimz.git
cd rimz
cargo xtask install
```

The command paths sit under `$(brew --prefix rustup)/bin` or `$HOME/.cargo/bin`. A `cargo` or `rustc` under `/opt/homebrew/bin` means Homebrew Rust still shadows rustup — see [`can't find crate for core`](#cant-find-crate-for-core).

## Verify your install

With `rimz` on your `PATH`, two commands confirm the build and the runtime.

```sh
rimz --version
rimz doctor
```

`rimz doctor` reports the multiplexer it selected, its version and whether it clears the floor, the presence-plugin status, sidebar liveness, and runtime socket headroom — the fastest read on whether a fresh machine is ready. From here, [set up your machine](./setup.md) is the next step: config init, agent hooks, and the settings that make the room comfortable.

## Configure your multiplexer

Rimz configures each room for you. On every session birth and reattach it sets the options agents need, so a freshly installed Zellij or tmux works without editing `~/.config/zellij/config.kdl` or `~/.tmux.conf`. A Zellij room opens in locked mode so your typing reaches the agent pane, with single-click sidebar jumps, 100k-line scrollback, the system clipboard, and resurrection off; a tmux room runs with the mouse on, focus events, OSC passthrough for desktop notifications, 100k-line history, and CSI-u extended keys, so Shift+Enter and Alt+Enter reach agents as soft newlines. On tmux 3.5.x, that trades clean multiline clipboard paste until tmux 3.6. Rimz reasserts these on every attach.

Your own Zellij and tmux config still owns everything Rimz leaves alone — the theme, true-color output, your default shell, copy-mode keybindings, and status-bar styling — and a baseline keeps the multiplexer pleasant in your sessions outside Rimz too. Tuning the room from Rimz's config (`[zellij]` and `[tmux]` overrides) and building that baseline are both covered in [set up your machine](./setup.md#configure-your-multiplexer).

## Uninstall

Run `rimz uninstall --all` from outside a Rimz room to remove installed hooks, live rooms, runtime/cache/data roots, durable state, per-machine config, and the installed binary. Project-local `.rimz/` dirs and Rimz-owned worktrees stay in place for manual review.

## Troubleshooting

### `can't find crate for core`

This error during `cargo xtask install` means the compiler Cargo used could not find the Rust standard library for `wasm32-wasip1`.

```text
error[E0463]: can't find crate for `core`
  = note: the `wasm32-wasip1` target may not be installed
```

Check the compiler and the target library that compiler sees.

```sh
command -v cargo
command -v rustc
rustup target list --installed | grep wasm32 || true
rustc --print target-libdir --target wasm32-wasip1
ls "$(rustc --print target-libdir --target wasm32-wasip1 2>/dev/null)" 2>/dev/null | grep libcore || true
```

A healthy setup prints a `libcore-*.rlib` file from that last command, and `cargo` and `rustc` resolve under `$HOME/.cargo/bin` or `$(brew --prefix rustup)/bin`.

On macOS, a common broken shape is `rustup target list --installed` showing `wasm32-wasip1` while `command -v rustc` points at `/opt/homebrew/bin/rustc`. That pairs rustup's target registry with Homebrew's compiler. Repair the shell so rustup's `cargo` and `rustc` come first, then rerun `cargo xtask install`.

```sh
RUSTUP_BIN="$(brew --prefix rustup)/bin"
export PATH="$RUSTUP_BIN:$PATH"
brew unlink rust || true
hash -r 2>/dev/null || rehash 2>/dev/null || true

command -v cargo
command -v rustc
ls "$(rustc --print target-libdir --target wasm32-wasip1)" | grep libcore
```

### `rimz doctor` flags the multiplexer as unsupported

Rimz refuses to start against a multiplexer too old to carry the room options it sets — tmux below 3.5, or Zellij below 0.44. Check the installed version, then install a current build from the per-OS steps above.

```sh
tmux -V
zellij --version
rimz doctor
```

On tmux, `extended-keys-format` (tmux 3.5) is the option an older server rejects; Rimz enables `extended-keys` and `*:extkeys` across supported tmux versions so Shift+Enter and Alt+Enter reach agents as soft newlines. tmux 3.5.x corrupts pasted newlines while extended keys are active; tmux 3.6 preserves paste too. On Zellij, 0.44 is the floor for single-click sidebar jumps and the presence-plugin topology channel.

### `rustup` is missing

Install `rustup` before running Rimz's source build.

```sh
# Linux
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

# macOS with Homebrew
brew install rustup
```

Then run:

```sh
cd /path/to/rimz
cargo xtask install
```
