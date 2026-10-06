#!/usr/bin/env bash
# Run from any directory. Corpus mutations go to ignored directories; checked-in
# seeds are copied, so fuzzing never silently edits the reviewed regression set.
set -euo pipefail
cd "$(dirname "$0")/.."
target="${1:-scheduler}"
# IO histories need a larger per-input deadline and sustained budget than policies.
case "$target" in
  history) budget=900; deadline=60; max_len=4096 ;;
  portfolio) budget=900; deadline=120; max_len=260 ;;
  notifications|queue) budget=600; deadline=60; max_len=4096 ;;
  *) budget=300; deadline=10; max_len=4096 ;;
esac
seconds="${2:-$budget}"
features="${3:-}"
case "$target" in scheduler|resources|inputs|queue|mempool|status|history|notifications|portfolio) ;; *) echo 'Target must be scheduler, resources, inputs, queue, mempool, status, history, notifications, or portfolio.' >&2; exit 2 ;; esac
if [[ ! "$seconds" =~ ^[1-9][0-9]*$ ]]; then echo 'Seconds must be a positive integer.' >&2; exit 2; fi
case "$features" in ''|zmq) ;; *) echo 'Features must be empty or zmq.' >&2; exit 2 ;; esac
feature_args=()
if [[ -n "$features" ]]; then feature_args=(--features "$features"); fi
# cargo-fuzz passes -seed through to libFuzzer. Every run prints it for replay.
fuzz_seed="${ENGINE_FUZZ_SEED:-1}"
if [[ ! "$fuzz_seed" =~ ^[0-9]+$ ]]; then echo 'ENGINE_FUZZ_SEED must be an integer.' >&2; exit 2; fi
# Every invocation has independent evidence, even when the RNG seed repeats.
report_parent="target/engine-exploration/fuzz/$target/${features:-default}/$fuzz_seed"
mkdir -p "$report_parent"
output=$(mktemp -d "$report_parent/$(git rev-parse --short HEAD)-XXXXXXXX")
echo "Campaign evidence: $output"
stage=setup
record_failure() {
  local campaign_exit=$?
  if (( campaign_exit != 0 )); then
    python3 scripts/engine-exploration-report.py incomplete --output "$output" \
      --stage "$stage" --exit-code "$campaign_exit" || true
  fi
}
trap record_failure EXIT
stage=seed-sync
python3 scripts/engine-exploration-report.py sync-seeds --seeds "fuzz/seeds/$target" --corpus "fuzz/corpus/$target"
stage=replay-settings
export ENGINE_SEMANTIC_REPORT="$PWD/$output/semantics"
python3 scripts/engine-exploration-report.py begin --output "$output" --corpus "fuzz/corpus/$target" \
  --seeds "fuzz/seeds/$target" --target "$target" --features "${features:-default}" \
  --seconds "$seconds" --timeout "$deadline" --max-len "$max_len" --seed "$fuzz_seed"
# Cargo-fuzz has no --locked build flag; reject lockfile drift before it builds.
stage=metadata
cargo metadata --manifest-path fuzz/Cargo.toml --locked --format-version 1 >/dev/null 2>"$output/metadata.log"
stage=build
cargo fuzz build --fuzz-dir fuzz "${feature_args[@]}" "$target" 2>&1 | tee "$output/build.log"
fuzz_host=$(rustc -vV | sed -n 's/^host: //p')
stage=calibration
ENGINE_SEMANTIC_REPORT="$PWD/$output/calibration-semantics" python3 scripts/engine-exploration-report.py calibrate --output "$output"  \
  --binary "fuzz/target/$fuzz_host/release/$target" --seeds "fuzz/seeds/$target" --timeout "$deadline"
stage=exploration
set +e
cargo fuzz run --fuzz-dir fuzz "${feature_args[@]}" "$target" -- \
  -max_total_time="$seconds" -max_len="$max_len" -timeout="$deadline" -rss_limit_mb=4096 \
  -seed="$fuzz_seed" -print_final_stats=1 2>&1 | tee "$output/fuzzer.log"
fuzz_exit=${PIPESTATUS[0]}
set -e
stage=reporting
python3 scripts/engine-exploration-report.py finish --output "$output" --corpus "fuzz/corpus/$target" --exit-code "$fuzz_exit"
exit "$fuzz_exit"
