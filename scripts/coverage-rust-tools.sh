#!/usr/bin/env bash
set -euo pipefail

# Run before each Rust coverage collection. Cargo install checks crates.io and
# replaces an older installed release without pinning the collector version.
rustup update nightly
rustup component add llvm-tools-preview --toolchain nightly
cargo +stable install cargo-llvm-cov --locked

rustc +nightly --version
cargo +nightly --version
cargo llvm-cov --version
