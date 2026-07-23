#!/bin/sh
set -eu

release_url="$(
    curl --proto '=https' --tlsv1.2 -fsSI \
        --retry 5 --retry-delay 2 --retry-all-errors \
        https://github.com/tmux/tmux/releases/latest \
        | tr -d '\r' \
        | sed -n 's|^[Ll]ocation: .*/tag/||p'
)"
version="${release_url##*/}"
if [ -z "$version" ]; then
    echo "could not resolve the latest tmux release" >&2
    exit 1
fi

echo "installing tmux ${version}"
curl --proto '=https' --tlsv1.2 -fL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "https://github.com/tmux/tmux/releases/download/${version}/tmux-${version}.tar.gz" \
    -o /tmp/tmux.tar.gz
mkdir -p /tmp/tmux-src
tar -xzf /tmp/tmux.tar.gz -C /tmp/tmux-src --strip-components=1
(
    cd /tmp/tmux-src
    ./configure --prefix=/usr/local
    make -j"$(nproc)"
    make install DESTDIR=/opt/stage
)
/opt/stage/usr/local/bin/tmux -V
