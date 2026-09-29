#!/usr/bin/env bash
set -euo pipefail

if ! command -v node >/dev/null 2>&1; then echo 'missing prerequisite: node' >&2; exit 2; fi
if ! command -v cargo >/dev/null 2>&1; then echo 'missing prerequisite: cargo' >&2; exit 2; fi
playwright=e2e/pos-playwright
if ! test -f "$playwright/node_modules/@playwright/test/package.json"; then
  echo "missing prerequisite: $playwright/node_modules (run npm ci there)" >&2; exit 2
fi
if ! test -f crates/monokulo/pos-ui/node_modules/vite/bin/vite.js; then
  echo 'missing prerequisite: POS Vite dependencies (run npm ci in crates/monokulo/pos-ui)' >&2; exit 2
fi

# Build the fixture server up front: each spec's beforeAll rebuilds it, and a
# cold build there overruns the 40s hook timeout.
cargo build --locked -p monokulo --example coverage_fixture
node "$playwright/prepare-coverage-assets.js"
export COVERAGE_ASSETS_DIR="$COVERAGE_OUTPUT/assets"
export COVERAGE_RAW_DIR="$COVERAGE_OUTPUT/raw"
export COVERAGE_INSTRUMENT=1
export COVERAGE_SCREENSHOTS=1
screenshots_dir="$(dirname "$COVERAGE_OUTPUT")/screenshots"
rm -rf "$screenshots_dir"
mkdir -p "$COVERAGE_RAW_DIR"
status=0
(cd "$playwright" && ./node_modules/.bin/playwright test -c coverage-browser.config.js) || status=$?
# The same browser code against the real engine and monokulo binaries (and a
# fake monerod): slower, but it is what merchants actually run. Adds its
# stages (the Logs page, the POS session timeline, store Diagnostics) to the
# gallery and its instrumented frames to the browser coverage. It runs even
# when the first suite failed, so one run reports every failure.
(cd "$playwright" && ./node_modules/.bin/playwright test -c coverage-real-binaries.config.js) || status=$?
if test "$status" != 0; then exit "$status"; fi
node "$playwright/collect-browser-report.js"
