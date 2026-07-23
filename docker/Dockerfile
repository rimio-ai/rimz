ARG BASE_IMAGE=debian:13-slim

FROM ${BASE_IMAGE} AS rimz-build

ARG DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential \
      ca-certificates \
      curl \
      git \
      libssl-dev \
      pkg-config \
 && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -fsSL \
      --retry 5 --retry-delay 2 --retry-all-errors \
      https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable \
 && rustc --version \
 && cargo --version

WORKDIR /src
COPY . .
RUN cargo build --locked --release -p rimz \
 && target/release/rimz --version

FROM ${BASE_IMAGE} AS tmux-build

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      bison \
      build-essential \
      ca-certificates \
      curl \
      libevent-dev \
      libncurses-dev \
      pkg-config \
 && rm -rf /var/lib/apt/lists/*

COPY --chmod=0755 docker/install-tmux.sh /usr/local/bin/install-tmux
RUN /usr/local/bin/install-tmux

FROM ${BASE_IMAGE}

ARG BASE_IMAGE
ARG DEBIAN_FRONTEND=noninteractive
LABEL org.opencontainers.image.source="https://github.com/rimio-ai/rimz" \
      org.opencontainers.image.description="Ready-to-run RimZ room with current multiplexers and priority agent CLIs" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.base.name="${BASE_IMAGE}"

ENV LANG=C.UTF-8 \
    PATH=/home/rimz/.local/bin:/home/rimz/.opencode/bin:$PATH

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      git \
      jq \
      libevent-core-2.1-7t64 \
      libncursesw6 \
      libstdc++6 \
      libutempter0 \
      openssh-client \
      procps \
      sudo \
      unzip \
      wget \
      xz-utils \
 && install -m 0755 -d /etc/apt/keyrings \
 && curl --proto '=https' --tlsv1.2 -fL \
      --retry 5 --retry-delay 2 --retry-all-errors \
      https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
 && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
 && printf 'deb [arch=%s signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\n' "$(dpkg --print-architecture)" \
      > /etc/apt/sources.list.d/github-cli.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends gh \
 && rm -rf /var/lib/apt/lists/*

COPY --from=tmux-build /opt/stage/usr/local/ /usr/local/
COPY --from=rimz-build /src/target/release/rimz /usr/local/bin/rimz
COPY --chmod=0755 docker/install-tools.sh /usr/local/bin/install-tools
RUN /usr/local/bin/install-tools \
 && npm install -g @earendil-works/pi-coding-agent \
 && npm cache clean --force \
 && pi --version

RUN userdel -r ubuntu 2>/dev/null || true \
 && useradd -m -u 1000 -s /bin/bash rimz \
 && printf 'rimz ALL=(ALL) NOPASSWD:ALL\n' > /etc/sudoers.d/rimz \
 && chmod 0440 /etc/sudoers.d/rimz \
 && printf 'export PATH="$HOME/.local/bin:$HOME/.opencode/bin:$PATH"\n' > /etc/profile.d/rimz-path.sh \
 && install -d -o rimz -g rimz /workspace

COPY --chmod=0755 docker/install-agents.sh /usr/local/bin/install-agents
COPY --chmod=0755 docker/entrypoint.sh /usr/local/bin/rimz-entrypoint

USER rimz
ENV HOME=/home/rimz
WORKDIR /workspace
RUN /usr/local/bin/install-agents \
 && rimz hooks install claude \
 && rimz hooks install codex \
 && rimz hooks install pi \
 && rimz hooks install opencode \
 && rimz --version \
 && tmux -V \
 && zellij --version \
 && ttyd --version \
 && node --version \
 && git --version \
 && gh --version \
 && claude --version \
 && codex --version \
 && pi --version \
 && opencode --version \
 && test "$(id -u)" -eq 1000 \
 && sudo -n true

EXPOSE 8200
ENTRYPOINT ["/usr/local/bin/rimz-entrypoint"]
CMD ["shell"]
