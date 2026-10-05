#!/usr/bin/env bash
# Regular generated cases AND explicit large fixtures. Separate processes and
# a single worker prevent the scale suite from measuring its own competition.
set -euo pipefail
cd "$(dirname "$0")/.."
features=${1:-default}
case "$features" in
  default) feature_args=() ;;
  zmq) feature_args=(--features zmq) ;;
  *) echo 'usage: scripts/engine-scale.sh [default|zmq]' >&2; exit 2 ;;
esac
export PROPTEST_CASES=${PROPTEST_CASES:-32}
export PROPTEST_RNG_SEED=${PROPTEST_RNG_SEED:-24601}
output="target/engine-scale/$features"
mkdir -p "$output"
{
  echo "revision=$(git rev-parse HEAD)"
  echo "features=$features cases=$PROPTEST_CASES seed=$PROPTEST_RNG_SEED"
  rustc --version
} > "$output/replay.txt"
cargo nextest run -p engine --lib --locked --profile ci --run-ignored all --test-threads 1 \
  "${feature_args[@]}" -E 'test(::scale::)' 2>&1 | tee "$output/tests.log"
cp target/nextest/ci/junit.xml "$output/junit.xml"
