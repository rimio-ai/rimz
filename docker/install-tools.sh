#!/bin/sh
set -eu

case "$(uname -m)" in
    x86_64)
        release_arch=x86_64
        node_arch=x64
        ;;
    aarch64)
        release_arch=aarch64
        node_arch=arm64
        ;;
    *)
        echo "unsupported architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

echo "installing latest ttyd for ${release_arch}"
curl --proto '=https' --tlsv1.2 -fL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "https://github.com/tsl0922/ttyd/releases/latest/download/ttyd.${release_arch}" \
    -o /usr/local/bin/ttyd
chmod 0755 /usr/local/bin/ttyd

echo "installing latest zellij for ${release_arch}"
curl --proto '=https' --tlsv1.2 -fL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "https://github.com/zellij-org/zellij/releases/latest/download/zellij-${release_arch}-unknown-linux-musl.tar.gz" \
    | tar -xz -C /usr/local/bin zellij

node_version="$(
    curl --proto '=https' --tlsv1.2 -fL \
        --retry 5 --retry-delay 2 --retry-all-errors \
        https://nodejs.org/dist/index.json \
        | jq -er 'map(select(.lts != false))[0].version'
)"
echo "installing Node.js ${node_version} LTS for ${node_arch}"
curl --proto '=https' --tlsv1.2 -fL \
    --retry 5 --retry-delay 2 --retry-all-errors \
    "https://nodejs.org/dist/${node_version}/node-${node_version}-linux-${node_arch}.tar.xz" \
    | tar -xJ -C /usr/local --strip-components=1

ttyd --version
zellij --version
node --version
npm --version
