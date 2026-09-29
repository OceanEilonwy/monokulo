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
# nextest (.config/nextest.toml's ci profile) runs the test binaries side by
# side and leaves a JUnit report, kept next to the coverage for the job
# summary whether or not the tests passed.
status=0
rm -f "${CARGO_TARGET_DIR:-target}/nextest/ci/junit.xml"
cargo +nightly llvm-cov nextest --workspace --locked --branch --html \
  --output-dir "$COVERAGE_OUTPUT" --exclude xtask --profile ci || status=$?
junit="${CARGO_TARGET_DIR:-target}/nextest/ci/junit.xml"
if test -f "$junit"; then cp "$junit" "$COVERAGE_OUTPUT/junit.xml"; fi
if test "$status" != 0; then exit "$status"; fi
cargo +nightly llvm-cov report --branch --json \
  --output-path "$COVERAGE_OUTPUT/raw.json"

# cargo-llvm-cov always adds an html/ child to --output-dir. Put its contents
# at rust/ so the public report path is stable.
mv "$COVERAGE_OUTPUT/html/"* "$COVERAGE_OUTPUT/"
rmdir "$COVERAGE_OUTPUT/html"

test -s "$COVERAGE_OUTPUT/index.html"
test -s "$COVERAGE_OUTPUT/raw.json"
