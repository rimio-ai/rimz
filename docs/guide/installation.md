# Installation

RimZ is a single binary for macOS and Linux. This page puts it on your machine with a current Zellij or tmux underneath it, and ends with a clean `rimz doctor`; [set up your machine](./setup.md) takes it from there.

Bring macOS or Linux, Git (RimZ shells out to it for branch and worktree work), and whichever agent CLIs you plan to run. The install script below is the path to take. [Other ways to install](#other-ways-to-install) covers Homebrew, a prebuilt archive, Cargo, and a source build, and the [Docker image](#run-in-docker) is a container with the whole toolchain already in it.

## Install RimZ

```sh
curl -fsSL https://raw.githubusercontent.com/rimio-ai/rimz/main/scripts/install.sh | sh
```

The script detects your platform, downloads the matching release archive, verifies it against the release's `SHA256SUMS`, and installs `rimz` into `/usr/local/bin` when that directory is writable and `~/.local/bin` otherwise. It finishes by printing the installed path and version, the `export PATH=` line to add when the destination is not already on your `PATH`, a note if another `rimz` earlier on `PATH` shadows the new one, and `Next: rimz doctor`.

On macOS it hands a default install to Homebrew whenever `brew` is on the machine, so RimZ upgrades and uninstalls through the package manager you already use. It takes the direct download instead when you set `RIMZ_VERSION` to anything but `latest`, set `RIMZ_INSTALL_DIR`, set `RIMZ_NO_BREW=1`, or already have a `rimz` on `PATH` that Homebrew did not install and does not know about. A Homebrew install that fails falls back to the release download rather than stopping.

`RIMZ_INSTALL_DIR`, `RIMZ_VERSION`, and `RIMZ_NO_BREW` go on the right of the pipe, where `sh` reads them:

```sh
curl -fsSL https://raw.githubusercontent.com/rimio-ai/rimz/main/scripts/install.sh \
  | RIMZ_INSTALL_DIR="$HOME/bin" RIMZ_VERSION=v0.4.1 sh
```

`RIMZ_VERSION` takes any release tag, or `latest-main` for a rolling build of the default branch.

Prebuilt archives cover Linux x86_64 against glibc, and macOS on Apple silicon and Intel. On ARM Linux or musl Linux the script stops and names its replacement, `cargo install --locked rimz`; see [Cargo](#cargo) below.

## Install Zellij or tmux

A RimZ room is one Zellij or tmux session: your agents running in panes, RimZ's sidebar docked down the left. RimZ drives that session rather than shipping its own terminal, so one of these has to be on the machine:

- Zellij 0.44.2 or newer, or
- tmux 3.5 or newer, with 3.6 or newer preferred. On 3.5.x, the soft-newline key support RimZ turns on corrupts multiline text pasted into an agent; 3.6 fixes the paste.

One is enough and both are first-class. `rimz start` refuses to open a room on a Zellij below the minimum and names the upgrade. An old tmux is not blocked at start, so check it yourself: `rimz doctor` reports the backend it selected and whether the version clears the minimum, which it labels the floor.

Packaged builds lag. Debian 12 ships tmux 3.3a and Debian 13 ships 3.5a, and most distributions do not package Zellij at all. On macOS, Homebrew's builds of both are current:

```sh
brew install zellij
brew install tmux         # one is enough; keep both to choose per project
```

On Linux, Zellij publishes a prebuilt binary with each release; drop it into your `PATH` (swap `x86_64` for `aarch64` on ARM):

```sh
curl -L https://github.com/zellij-org/zellij/releases/latest/download/zellij-x86_64-unknown-linux-musl.tar.gz \
  | sudo tar -xz -C /usr/local/bin zellij
zellij --version
```

For tmux, try the distribution package first and check what it gives you:

```sh
sudo apt install tmux     # Debian/Ubuntu; dnf and pacman ship it too
tmux -V
```

tmux publishes no prebuilt binaries, so when the packaged one is below the floor, install a current one through Homebrew on Linux or build the release tarball. The first line installs the build dependencies:

```sh
sudo apt install -y build-essential pkg-config libevent-dev libncurses-dev bison
curl -fsSLO https://github.com/tmux/tmux/releases/download/3.7/tmux-3.7.tar.gz
tar -xzf tmux-3.7.tar.gz && cd tmux-3.7
./configure && make -j"$(nproc)" && sudo make install
```

Nothing needs configuring for RimZ. Every room applies the options agents need on session start and reattach, including the key bindings that turn Shift+Enter and Alt+Enter into soft newlines; your theme, shell, and the rest of your keybinds stay as they are. What the room asserts, and a baseline worth keeping for your own sessions, are both in [Zellij and tmux](./multiplexer.md).

## Install the agent CLIs

RimZ drives the stock agent CLIs and neither bundles nor replaces them. Install the ones you use from their own docs, and RimZ picks them up: `rimz doctor` names the agents it found on this machine and the ones it did not. Built-in adapters cover thirteen agents, Claude Code and Codex first among them; [agent support](../reference/agent-support.md) is the per-agent list of what a card shows and where the gaps are.

## Verify the install

```sh
rimz --version
rimz doctor
```

`rimz doctor` reads the whole machine in one pass: the multiplexer it selected and whether the version clears the floor, the agent CLIs it found and the state of their hooks, the terminal's color depth, and the health of RimZ's own files. It starts, stops, or moves nothing in the room, so run it as often as you like. Every row it can print, and what to do about a `✗`, is in [troubleshooting](./troubleshooting.md#reading-the-report).

Next: [set up your machine](./setup.md), which writes the per-machine config, installs the agent hooks that let the sidebar see your agents, and opens your first room.

## Truecolor terminal and a Nerd Font (optional)

RimZ's `modern` style paints in 24-bit color and draws with Nerd Font icons. The two halves are independent: RimZ probes for them separately, so you can take truecolor without the icons or the other way round, and plain indexed colors with Unicode glyphs read fine at any depth.

Truecolor is on by default in Ghostty, kitty, WezTerm, iTerm2, and Alacritty. The icons need a [Nerd Font](https://www.nerdfonts.com/font-downloads) installed and selected as your terminal font. On macOS, Homebrew installs a terminal and two patched fonts in one pass:

```sh
brew install --cask ghostty
brew install --cask font-cascadia-code-nf font-jetbrains-maple-mono-nf
```

Ghostty and kitty add one thing more. They speak the kitty graphics protocol, which is what draws a [sidebar pet](./pets.md) as crisp pixels rather than cell art. In a tmux room it also takes tmux 3.6 or newer with `allow-passthrough on`. A Zellij room stays on cell art whatever the terminal, because Zellij does not support the placement mode RimZ's pixel painter uses.

Select the font in your terminal before [set up your machine](./setup.md) runs its probes; if you have already been through setup, `rimz setup` reruns them and picks the icons up. The color-depth and glyph settings are in [theming](./theme.md#style-preset).

## Other ways to install

### Homebrew (macOS)

```sh
brew install rimio-ai/rimz/rimz
```

Homebrew adds the `rimio-ai/rimz` tap as part of that command. From then on `rimz update` refreshes formula data and upgrades `rimio-ai/rimz/rimz` for you.

### A prebuilt binary

Every [release](https://github.com/rimio-ai/rimz/releases) ships one archive per platform plus a `SHA256SUMS` file:

| Archive | Platform |
| --- | --- |
| `rimz-x86_64-unknown-linux-gnu.tar.gz` | Linux, x86_64 |
| `rimz-aarch64-apple-darwin.tar.gz` | macOS, Apple silicon |
| `rimz-x86_64-apple-darwin.tar.gz` | macOS, Intel |

Download, verify, and install. This is the Linux form; on macOS swap the archive name and verify with `shasum -a 256 -c --ignore-missing`:

```sh
curl -fsSLO https://github.com/rimio-ai/rimz/releases/latest/download/rimz-x86_64-unknown-linux-gnu.tar.gz
curl -fsSLO https://github.com/rimio-ai/rimz/releases/latest/download/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf rimz-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 rimz-x86_64-unknown-linux-gnu/rimz /usr/local/bin/rimz
```

Two platform caveats. The Linux binary links against a current glibc, so on an ARM Linux machine, or if the binary reports a `GLIBC` version error, use [Cargo](#cargo) instead. On macOS a `curl` download runs as it is, while a browser download picks up Gatekeeper's quarantine attribute, which `xattr -d com.apple.quarantine rimz` clears.

Alongside the tagged releases, the `latest-main` prerelease carries a rolling build of the default branch, for when you want a fix that has not been tagged yet.

### Cargo

```sh
cargo install --locked rimz
```

This builds RimZ from crates.io and works on every supported platform, ARM Linux included. It needs the Rust toolchain (through [rustup](https://rustup.rs)) and a C linker: `build-essential` on Debian and Ubuntu, `xcode-select --install` on macOS. The crate ships RimZ's Zellij plugin as a prebuilt WebAssembly artifact, so there is no extra Rust target to add.

### From source

Build from source to work on RimZ itself, or to reach a platform the paths above miss. Beyond a C toolchain and Git it needs Rust through `rustup`:

```sh
# Linux: build tools (Debian/Ubuntu shown; use dnf or pacman equivalents)
sudo apt install -y build-essential pkg-config git

# macOS: Apple's command-line tools
xcode-select --install

# Both: Rust through rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

Then clone and install:

```sh
git clone https://github.com/rimio-ai/rimz.git
cd rimz
cargo xtask install
```

`cargo xtask install` builds the Zellij presence plugin as a WebAssembly artifact, embeds it, builds `rimz`, copies the binary into `~/.cargo/bin`, and prints the installed version and path. [rust-toolchain.toml](../../rust-toolchain.toml) pins the channel, components, and the `wasm32-wasip1` target, and `rustup` applies it on the first build, so no target setup is needed by hand.

To share one build with every user on the machine, `cargo xtask install-system` publishes it to `/usr/local/bin/rimz` instead; run it without a `sudo` prefix, since it builds as you and uses sudo only for that last step, and remove that copy with `sudo rm /usr/local/bin/rimz`. The contributor build profiles, the gate stack, and the rest of the task surface are in [the Rust conventions](../contributing/rust-conventions.md).

### Onto a server you SSH into

`rimz remote setup <host>` installs RimZ on the other end of an SSH connection: it detects the host's OS and architecture, verifies the matching release archive there, and puts `rimz` in `~/.local/bin`. Running it again is also how you upgrade that host. The link, the reconnect policy, and what to do from there are in [remote](./remote.md).

## Run in Docker

A plain container gives you a clean shell and leaves you to assemble the tools. The RimZ image starts as uid 1000 with RimZ, a current Zellij and tmux, ttyd, GitHub CLI, Node.js, Claude Code, Codex, Pi, and OpenCode on `PATH`, hooks already installed:

```sh
docker run --rm -it -v "$PWD":/workspace ghcr.io/rimio-ai/rimz
```

The bind mount makes the current checkout the room at `/workspace`. Agent logins otherwise die with the container, so add a named home volume to keep logins, agent state, and RimZ configuration across runs:

```sh
docker run --rm -it \
  -v "$PWD":/workspace \
  -v rimz-home:/home/rimz \
  ghcr.io/rimio-ai/rimz
```

Codex keeps per-hook trust behind its own UI, so the image leaves that consent to you: after the first Codex launch, run `/hooks` inside Codex and trust the RimZ hooks. Until then `rimz doctor` reports them as installed but untrusted.

The image takes a command as its argument. With none it opens a shell; `web` opens the `/workspace` room, binds RimZ to every container interface, and leaves the authenticated ttyd daemon online; anything else runs as an ordinary command in the container:

```sh
docker run -d --name rimz-web \
  -p 8200:8200 \
  -v "$PWD":/workspace \
  -v rimz-home:/home/rimz \
  ghcr.io/rimio-ai/rimz web
docker logs rimz-web              # the room URL and its generated Basic credential
docker rm -f rimz-web             # stop and remove it
```

The default and `debian` tags run Debian 13, `ubuntu` runs Ubuntu 26.04 LTS, and a release carries its own `<version>`, `<version>-debian`, and `<version>-ubuntu` tags. The floating tags rebuild daily against the newest release so the bundled toolchain stays current; pin an image digest when the filesystem has to stay immutable. Both amd64 and arm64 are published.

The uid-1000 `rimz` user has passwordless sudo, so the same image works as a devcontainer base:

```json
{
  "image": "ghcr.io/rimio-ai/rimz",
  "remoteUser": "rimz"
}
```

Use a host install instead when you need an agent CLI the image does not carry, direct access to a multiplexer already running on the host, or native desktop integration.

## Update

One command updates RimZ whichever way you installed it:

```sh
rimz update
```

`update` reads the install path of the binary it is running from and reuses it. A Homebrew install refreshes formula data before upgrading the formula, so stale tap data cannot hide a released build, and a Cargo install reruns `cargo install --locked rimz`. A script or downloaded binary resolves the latest release, verifies the archive against `SHA256SUMS`, requires the extracted binary's `--version` to pass, then atomically replaces the running binary in place. When the binary changes, the new build runs `rimz reload`, so live sidebars and any refreshing `rimz stats` dashboard move onto it without a pane changing.

`--version <TAG>` picks a release instead: an older tag to roll back, or `latest-main` to follow the rolling build. Cargo takes numbered tags only, normalizing a short `v0.4` to `0.4.0`. Homebrew picks its own formula version and refuses the flag, so to hold a machine on one release, install it standalone with the script's `RIMZ_VERSION`. The per-origin table and every refusal message are in the [maintenance reference](../reference/cli/maintenance.md#update-rimz).

## Uninstall

Run this from outside a RimZ room; inside one it refuses and tells you to detach first.

```sh
rimz uninstall --all
```

It prints every root, room, hook, and binary it is about to touch and waits for a `y`. On that confirmation it removes, reporting each one as it goes:

- the RimZ hooks and skill links in each agent's own home,
- the live rooms on both backends and the loop timer,
- the runtime tree and the caches under `~/.rimz`,
- the durable stores and spend history,
- the per-machine config, themes, and trust grants,
- every `rimz` binary it can find.

Four things survive on purpose: your agent library (`agents/`, `subagents/`, `teams/`, `traits/`, `skills/`, `profiles/`) and `handoffs/`; the provider account homes under `~/.rimz/accounts/`, whose credentials belong to the provider; project-local `.rimz/` directories; and RimZ-owned worktrees, which can hold unlanded work. A Homebrew install needs `brew uninstall rimz` as well. Partial removals through `--state`, `--config`, and `--keep-binary`, and the root-by-root table of what each one takes, are in the [maintenance reference](../reference/cli/maintenance.md#uninstall-rimz).

## Troubleshooting

### `GLIBC_x.xx not found` running the prebuilt Linux binary

The release binary links against a current glibc, and an older distribution cannot load it. Install through [Cargo](#cargo) instead, which builds against the glibc you have.

### `can't find crate for core` during a source build

This error during `cargo xtask install` means the compiler Cargo used cannot see the Rust standard library for `wasm32-wasip1`:

```text
error[E0463]: can't find crate for `core`
  = note: the `wasm32-wasip1` target may not be installed
```

Usually a non-rustup Rust is shadowing rustup's on your `PATH`; on macOS, Homebrew's `rust` formula is the common culprit. Check where the tools resolve:

```sh
command -v cargo
command -v rustc
rustup show active-toolchain
```

A healthy setup resolves both under `$HOME/.cargo/bin`, or under rustup's own bin directory. If they point elsewhere, put rustup first and remove the shadowing toolchain, then rerun `cargo xtask install`:

```sh
brew unlink rust        # macOS, if Homebrew's rust formula is installed
hash -r
```

Every symptom after the install, starting with [a multiplexer flagged as too old](./troubleshooting.md#zellij-or-tmux-is-missing-or-too-old), has its fix in the [troubleshooting guide](./troubleshooting.md).
