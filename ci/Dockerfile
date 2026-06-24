FROM node:24-trixie

ARG ZELLIJ_VERSION=0.44.3
ARG MOLD_VERSION=2.41.0
ARG ZIG_VERSION=0.16.0
ARG ZIG_SHA256=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
ARG ZIG_DOWNLOAD_URL=
ARG APPLE_CODESIGN_VERSION=0.29.0
ARG APPLE_CODESIGN_SHA256=dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002
ARG CARGO_VET_VERSION=0.10.2

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
      tmux git gh curl ca-certificates xz-utils python3 \
      build-essential pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/* \
 && tmux -V

RUN curl --proto '=https' --tlsv1.2 -fsSL --retry 5 --retry-delay 2 --retry-all-errors https://sh.rustup.rs | \
      sh -s -- -y --no-modify-path --default-toolchain stable \
 && rustup component add rustfmt clippy llvm-tools-preview \
 && rustup target add wasm32-wasip1 aarch64-apple-darwin x86_64-apple-darwin \
 && rustc -V && cargo -V

RUN curl --proto '=https' --tlsv1.2 -fsSL --retry 5 --retry-delay 2 --retry-all-errors \
      https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash \
 && cargo binstall -y --no-symlinks \
      cargo-nextest cargo-deny cargo-machete cargo-llvm-cov cargo-semver-checks cargo-zigbuild \
 && cargo install --locked cargo-vet@${CARGO_VET_VERSION}

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

RUN rustc -V \
 && cargo nextest --version \
 && cargo deny --version \
 && cargo machete --version \
 && cargo llvm-cov --version \
 && cargo semver-checks --version \
 && cargo vet --version \
 && cargo zigbuild --help >/dev/null \
 && tmux -V \
 && zellij --version \
 && mold --version \
 && zig version \
 && rcodesign --version \
 && python3 -V \
 && gh --version \
 && node -v \
 && printf 'rimz-ci\n' > /etc/rimz-ci
