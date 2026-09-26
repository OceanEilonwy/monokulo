#!/usr/bin/env bash
set -euo pipefail

if ! command -v rustup >/dev/null 2>&1; then
  echo 'missing prerequisite: rustup' >&2
  exit 2
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo 'missing prerequisite: cargo' >&2
  exit 2
fi

bash scripts/coverage-rust-tools.sh

# Both reports use the same profile data. The report subcommand never reruns
# tests. The normal cargo-llvm-cov exclusion rules omit test and vendor source.
cargo +nightly llvm-cov --workspace --locked --branch --html \
  --output-dir "$COVERAGE_OUTPUT" --exclude xtask
cargo +nightly llvm-cov report --branch --json \
  --output-path "$COVERAGE_OUTPUT/raw.json"

# cargo-llvm-cov always adds an html/ child to --output-dir. Put its contents
# at rust/ so the public report path is stable.
mv "$COVERAGE_OUTPUT/html/"* "$COVERAGE_OUTPUT/"
rmdir "$COVERAGE_OUTPUT/html"

test -s "$COVERAGE_OUTPUT/index.html"
test -s "$COVERAGE_OUTPUT/raw.json"
