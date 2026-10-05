#!/usr/bin/env bash
# Run from any directory. Corpus mutations go to ignored directories; checked-in
# seeds are copied, so fuzzing never silently edits the reviewed regression set.
set -euo pipefail
cd "$(dirname "$0")/.."
target="${1:-scheduler}"
seconds="${2:-60}"
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
cargo fuzz run --fuzz-dir fuzz "${feature_args[@]}" "$target" -- \
  -max_total_time="$seconds" -max_len=4096 -timeout=10 -rss_limit_mb=4096 \
  -seed="$fuzz_seed" -print_final_stats=1
