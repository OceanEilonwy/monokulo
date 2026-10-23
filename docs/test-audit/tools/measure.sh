#!/usr/bin/env bash
# Measured Rust coverage for the test audit. Serialised with flock: every call
# shares target/llvm-cov-target, so only one runs at a time (others wait).
#
#   measure.sh alone   '<nextest filterset>'   lines/branches the matching tests cover on their own
#   measure.sh without '<nextest filterset>'   lines the full suite covers that the suite minus
#                                              the matching tests does not (lost if deleted)
#
# Filtersets are nextest's: e.g. "test(=views::order::tests::shows_order_id)",
# "binary_id(monokulo) & test(/^views::/)". Output is JSON on stdout.
# Baseline (full suite llvm-cov JSON) is target/test-audit/baseline.json.
set -euo pipefail
root=/home/henry/Downloads/mokulo/.claude/worktrees/test-audit
out=$root/target/test-audit/measure
mkdir -p "$out"
mode=$1; filter=$2
case $mode in
  alone) expr="$filter" ;;
  without) expr="not ($filter)" ;;
  *) echo "usage: measure.sh alone|without '<filterset>'" >&2; exit 2 ;;
esac
tag=$(printf '%s' "$mode $filter" | sha1sum | cut -c1-12)
exec 9>"$out/.lock"
flock 9
cd "$root"
export CARGO_PROFILE_DEV_OPT_LEVEL=0
cargo +nightly llvm-cov nextest --workspace --locked --exclude xtask \
  --profile ci --features monokulo/snp --no-fail-fast -E "$expr" \
  --branch --json --output-path "$out/$tag.json" >"$out/$tag.log" 2>&1 || true
python3 -I "$root/target/test-audit/tools/lcov_diff.py" "$mode" "$out/$tag.json" \
  "$root/target/test-audit/baseline.json" "$filter" "$out/$tag.log"
