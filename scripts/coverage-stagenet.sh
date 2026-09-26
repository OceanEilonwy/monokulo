#!/usr/bin/env bash
set -euo pipefail

if ! command -v node >/dev/null 2>&1; then echo 'missing prerequisite: node' >&2; exit 2; fi
if ! command -v cargo >/dev/null 2>&1; then echo 'missing prerequisite: cargo' >&2; exit 2; fi
playwright=e2e/pos-playwright
if ! test -f "$playwright/node_modules/@playwright/test/package.json"; then
  echo "missing prerequisite: $playwright/node_modules (run npm ci there)" >&2; exit 2
fi
node "$playwright/prepare-coverage-assets.js"
export COVERAGE_PROFILE=stagenet
export COVERAGE_INSTRUMENT=1
export COVERAGE_SCREENSHOTS=1
export COVERAGE_RAW_DIR="$COVERAGE_OUTPUT/raw"
mkdir -p "$COVERAGE_RAW_DIR"
(cd "$playwright" && ./node_modules/.bin/playwright test -c coverage-stagenet.config.js)
node "$playwright/collect-browser-report.js"
