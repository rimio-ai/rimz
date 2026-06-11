# Installation

Rimz source installs build one host binary and the Zellij presence plugin that ships inside it. The host binary builds for your machine, and the presence plugin builds for `wasm32-wasip1`, so the Rust compiler that Cargo runs must have that target's standard library available.

Use `rustup` for source installs. The repo pins the stable channel, required components, and required targets in [rust-toolchain.toml](../../rust-toolchain.toml), and `rustup` applies that file when `cargo` and `rustc` come from a rustup-managed toolchain.

## Linux

Install system build tools, Git, and one supported multiplexer.

```sh
# Debian or Ubuntu
sudo apt update
sudo apt install -y build-essential pkg-config git tmux

# Fedora
sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config git tmux

# Arch
sudo pacman -S --needed base-devel pkgconf git tmux
```

Install Rust through `rustup`.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Clone and install Rimz.

```sh
git clone https://github.com/rimz/rimz.git
cd rimz
cargo xtask install
```

Verify the selected Rust toolchain when installation fails before compiling Rimz.

```sh
command -v cargo
command -v rustc
rustup show active-toolchain
rustc --print target-libdir --target wasm32-wasip1
ls "$(rustc --print target-libdir --target wasm32-wasip1)" | grep libcore
```

`cargo` and `rustc` normally resolve under `$HOME/.cargo/bin`, and the final command prints a `libcore-*.rlib` file.

## macOS

Install Apple's command-line tools, Homebrew when you use it, and one supported multiplexer.

```sh
xcode-select --install
brew install git tmux
```

Install `rustup`. The Homebrew route keeps rustup itself under Homebrew while rustup owns the Rust toolchains.

```sh
brew install rustup
```

Put Homebrew rustup's proxy directory before Homebrew's general bin directory.

```sh
RUSTUP_BIN="$(brew --prefix rustup)/bin"
grep -qxF "export PATH=\"$RUSTUP_BIN:\$PATH\"" ~/.zshrc || \
  echo "export PATH=\"$RUSTUP_BIN:\$PATH\"" >> ~/.zshrc
exec zsh -l
```

Initialize and provision the repo's toolchain.

```sh
rustup default stable
rustup component add rustfmt clippy llvm-tools-preview
rustup target add wasm32-wasip1 aarch64-apple-darwin x86_64-apple-darwin
```

Homebrew's `rust` formula provides a separate `cargo` and `rustc`. Keep it out of the shell path for Rimz source builds, because `rustup target add` installs targets for rustup toolchains, not Homebrew's compiler.

```sh
brew unlink rust || true
hash -r 2>/dev/null || rehash 2>/dev/null || true
```

Verify that Cargo and rustc resolve through rustup, then install Rimz.

```sh
command -v cargo
command -v rustc
rustup show active-toolchain
rustc --print target-libdir --target wasm32-wasip1
ls "$(rustc --print target-libdir --target wasm32-wasip1)" | grep libcore

git clone https://github.com/rimz/rimz.git
cd rimz
cargo xtask install
```

The expected command paths are under `$(brew --prefix rustup)/bin` or `$HOME/.cargo/bin`. Paths under `/opt/homebrew/bin/cargo` and `/opt/homebrew/bin/rustc` mean Homebrew Rust still shadows rustup.

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
