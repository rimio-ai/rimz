FROM node:24-trixie

ARG ZELLIJ_VERSION=0.44.3
ARG TMUX_VERSION=3.7
ARG TMUX_SHA256=2344f191501b8a73eb71dd6c5fd5dcf8c765f5066f34ab46f04b3013dc7bc1a5
ARG PYTHON_VERSION=3.14
ARG UV_VERSION=0.11.25
ARG UV_SHA256=1db18b5e76fa645a7f3865773139bdec8e2d46adbdbb35e7410b34fa8015ccd2
ARG MOLD_VERSION=2.41.0
ARG ZIG_VERSION=0.16.0
ARG ZIG_SHA256=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
ARG ZIG_DOWNLOAD_URL=
ARG APPLE_CODESIGN_VERSION=0.29.0
ARG APPLE_CODESIGN_SHA256=dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002
ARG CARGO_VET_VERSION=0.10.2
ARG SCCACHE_VERSION=0.16.0

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# tmux and Python come from source/uv below, not apt: Debian trixie ships
# tmux 3.5a and Python 3.13, both below the versions CI targets. libevent,
# ncurses, and bison are the tmux build deps (build-essential/pkg-config cover
# the rest).
RUN apt-get update && apt-get install -y --no-install-recommends \
      git gh curl ca-certificates xz-utils \
      build-essential pkg-config libssl-dev \
      libevent-dev libncurses-dev bison \
 && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -fsSL --retry 5 --retry-delay 2 --retry-all-errors https://sh.rustup.rs | \
      sh -s -- -y --no-modify-path --default-toolchain stable \
 && rustup component add rustfmt clippy llvm-tools-preview \
 && rustup target add wasm32-wasip1 aarch64-apple-darwin x86_64-apple-darwin \
 && rustc -V && cargo -V

RUN curl --proto '=https' --tlsv1.2 -fsSL --retry 5 --retry-delay 2 --retry-all-errors \
      https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash \
 && cargo binstall -y --no-symlinks \
      cargo-nextest cargo-deny cargo-machete cargo-llvm-cov cargo-semver-checks cargo-zigbuild sccache@${SCCACHE_VERSION} \
 && cargo install --locked cargo-vet@${CARGO_VET_VERSION}

# Bake a warm RustSec advisory DB so the Gitea deny gate reads it locally
# (`cargo deny check --disable-fetch`) instead of cloning per run. The per-run
# clone is the source of flaky FETCH_HEAD failures. CARGO_HOME above puts it at
# /usr/local/cargo/advisory-dbs, where the runtime gate resolves it.
# `cargo deny fetch db` requires a manifest in cwd, so use a throwaway crate;
# with no deny.toml it uses the default RustSec URL, matching the committed
# deny.toml because it sets no db-urls.
RUN tmp="$(mktemp -d)" \
 && printf '[package]\nname="x"\nversion="0.0.0"\nedition="2021"\n' > "$tmp/Cargo.toml" \
 && mkdir "$tmp/src" \
 && : > "$tmp/src/lib.rs" \
 && ( cd "$tmp" && cargo deny fetch db ) \
 && ( cd "$tmp" && cargo deny check --disable-fetch advisories ) \
 && rm -rf "$tmp" \
 && find /usr/local/cargo/advisory-dbs -maxdepth 1 -type d -name 'advisory-db-*' | grep -q .

RUN curl -fL --retry 5 --retry-delay 2 --retry-all-errors "https://github.com/zellij-org/zellij/releases/download/v${ZELLIJ_VERSION}/zellij-x86_64-unknown-linux-musl.tar.gz" \
      | tar -xz -C /usr/local/bin \
 && curl -fL --retry 5 --retry-delay 2 --retry-all-errors "https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/mold-${MOLD_VERSION}-x86_64-linux.tar.gz" \
      | tar -xz -C /usr/local --strip-components=1 \
 && zig_url="${ZIG_DOWNLOAD_URL:-https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz}" \
 && curl -fL --retry 5 --retry-delay 2 --retry-all-errors "$zig_url" -o /tmp/zig.tar.xz \
 && printf '%s  /tmp/zig.tar.xz\n' "$ZIG_SHA256" | sha256sum -c - \
 && mkdir -p /opt/zig \
 && tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
 && ln -s /opt/zig/zig /usr/local/bin/zig \
 && curl -fL --retry 5 --retry-delay 2 --retry-all-errors "https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign%2F${APPLE_CODESIGN_VERSION}/apple-codesign-${APPLE_CODESIGN_VERSION}-x86_64-unknown-linux-musl.tar.gz" -o /tmp/rcodesign.tar.gz \
 && printf '%s  /tmp/rcodesign.tar.gz\n' "$APPLE_CODESIGN_SHA256" | sha256sum -c - \
 && tar -xzf /tmp/rcodesign.tar.gz -C /usr/local/bin --strip-components=1 \
 && rm -f /tmp/zig.tar.xz /tmp/rcodesign.tar.gz

# tmux from source: Debian trixie ships 3.5a, below the pet pixel tier floor
# (tmux 3.6+) the live-backend and pet tests exercise. The pinned release
# tarball is a complete autotools dist.
RUN curl -fL --retry 5 --retry-delay 2 --retry-all-errors \
      "https://github.com/tmux/tmux/releases/download/${TMUX_VERSION}/tmux-${TMUX_VERSION}.tar.gz" -o /tmp/tmux.tar.gz \
 && printf '%s  /tmp/tmux.tar.gz\n' "$TMUX_SHA256" | sha256sum -c - \
 && mkdir -p /tmp/tmux-src \
 && tar -xzf /tmp/tmux.tar.gz -C /tmp/tmux-src --strip-components=1 \
 && ( cd /tmp/tmux-src && ./configure --prefix=/usr/local && make -j"$(nproc)" && make install ) \
 && rm -rf /tmp/tmux.tar.gz /tmp/tmux-src \
 && tmux -V

# Python from a uv-managed standalone build: Debian trixie ships 3.13, below
# the 3.14 floor. `uv python install --default` lands python/python3 shims in
# UV_PYTHON_BIN_DIR (/usr/local/bin), ahead of any apt python on PATH.
ENV UV_PYTHON_INSTALL_DIR=/opt/uv/python \
    UV_PYTHON_BIN_DIR=/usr/local/bin
RUN curl -fL --retry 5 --retry-delay 2 --retry-all-errors \
      "https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-x86_64-unknown-linux-gnu.tar.gz" -o /tmp/uv.tar.gz \
 && printf '%s  /tmp/uv.tar.gz\n' "$UV_SHA256" | sha256sum -c - \
 && tar -xzf /tmp/uv.tar.gz -C /usr/local/bin --strip-components=1 \
 && rm -f /tmp/uv.tar.gz \
 && uv python install "${PYTHON_VERSION}" --default \
 && python3 --version

# Bake the Cargo registry/index into the image so Gitea PR jobs can skip a
# shared writable rust-cache restore and fetch only lockfile drift.
COPY Cargo.toml Cargo.lock /tmp/registry-warm/
COPY crates/ /tmp/registry-warm/crates/
COPY xtask/ /tmp/registry-warm/xtask/
RUN ( cd /tmp/registry-warm && cargo fetch --locked ) \
 && rm -rf /tmp/registry-warm \
 && test -d /usr/local/cargo/registry/index/index.crates.io-1949cf8c6b5b557f

RUN rustc -V \
 && cargo nextest --version \
 && cargo deny --version \
 && cargo machete --version \
 && cargo llvm-cov --version \
 && cargo semver-checks --version \
 && cargo vet --version \
 && cargo zigbuild --help >/dev/null \
 && sccache --version \
 && uv --version \
 && tmux -V \
 && zellij --version \
 && mold --version \
 && zig version \
 && rcodesign --version \
 && python3 -V \
 && gh --version \
 && node -v \
 && printf 'rimz-ci\n' > /etc/rimz-ci
