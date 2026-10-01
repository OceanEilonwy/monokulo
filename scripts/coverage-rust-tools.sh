#!/usr/bin/env bash
set -euo pipefail

# Run before each Rust coverage collection. Cargo install checks crates.io and
# replaces an older installed release without pinning the collector version.
# CI installs the same tools as prebuilt binaries in its own steps
# (.github/workflows/ci.yml) and sets COVERAGE_TOOLS_PREINSTALLED=1, since
# building them from source here takes over a minute.
if test "${COVERAGE_TOOLS_PREINSTALLED:-}" != 1; then
  rustup update nightly
  rustup component add llvm-tools-preview --toolchain nightly
  cargo install cargo-llvm-cov --locked
  cargo install cargo-nextest --locked
fi

rustc +nightly --version
cargo +nightly --version
cargo llvm-cov --version
cargo nextest --version
