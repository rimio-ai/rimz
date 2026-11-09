#!/bin/sh

set -eu

cargo install --locked \
    cargo-insta \
    rust-code-analysis-cli
