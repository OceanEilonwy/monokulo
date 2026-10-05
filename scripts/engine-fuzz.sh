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
mkdir -p "fuzz/corpus/$target"
for seed_file in fuzz/seeds/"$target"/*; do
  seed_name="${seed_file##*/}"
  if [[ ! -e "fuzz/corpus/$target/$seed_name" ]]; then cp "$seed_file" "fuzz/corpus/$target/$seed_name"; fi
done
# cargo-fuzz passes -seed through to libFuzzer. Every run prints it for replay.
fuzz_seed="${ENGINE_FUZZ_SEED:-1}"
if [[ ! "$fuzz_seed" =~ ^[0-9]+$ ]]; then echo 'ENGINE_FUZZ_SEED must be an integer.' >&2; exit 2; fi
# Cargo-fuzz has no --locked build flag; reject lockfile drift before it builds.
cargo metadata --manifest-path fuzz/Cargo.toml --locked --format-version 1 >/dev/null
output="target/engine-exploration/fuzz/$target/${features:-default}/$fuzz_seed"
mkdir -p "$output"
export ENGINE_SEMANTIC_REPORT="$PWD/$output/semantics"
# A fresh report for each invocation; preserve raw observations for independent auditing.
rm -f "$output"/semantics.*.jsonl
python3 scripts/engine-exploration-report.py begin --output "$output" --corpus "fuzz/corpus/$target"  \
  --target "$target" --features "${features:-default}" --seconds "$seconds" --timeout "$deadline" --max-len "$max_len" --seed "$fuzz_seed"
cargo fuzz build --fuzz-dir fuzz "${feature_args[@]}" "$target"
fuzz_host=$(rustc -vV | sed -n 's/^host: //p')
ENGINE_SEMANTIC_REPORT="$PWD/$output/calibration-semantics" python3 scripts/engine-exploration-report.py calibrate --output "$output"  \
  --binary "fuzz/target/$fuzz_host/release/$target" --seeds "fuzz/seeds/$target" --timeout "$deadline"
set +e
cargo fuzz run --fuzz-dir fuzz "${feature_args[@]}" "$target" -- \
  -max_total_time="$seconds" -max_len="$max_len" -timeout="$deadline" -rss_limit_mb=4096 \
  -seed="$fuzz_seed" -print_final_stats=1 2>&1 | tee "$output/fuzzer.log"
fuzz_exit=${PIPESTATUS[0]}
set -e
python3 scripts/engine-exploration-report.py finish --output "$output" --corpus "fuzz/corpus/$target" --exit-code "$fuzz_exit"
exit "$fuzz_exit"
