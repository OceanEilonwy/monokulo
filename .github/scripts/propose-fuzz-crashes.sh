#!/usr/bin/env bash
# Opens a draft PR to main adding the crashing inputs the fuzz jobs found as
# reviewed seeds (fuzz/seeds/<target>/crash-<hash>). Merge it together with
# the fix: the seed fails the calibration until the bug is fixed.
# Usage: propose-fuzz-crashes.sh <directory the fuzz artifacts were downloaded to>
set -euo pipefail

artifacts=$(realpath "${1:?artifact directory}")
run=${GITHUB_RUN_ID:?}
branch=ci/fuzz-crash-$run

git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
git fetch --quiet origin main
git switch --quiet -c "$branch" origin/main

found=()
for crash in "$artifacts"/*/fuzz/artifacts/*/*; do
  [ -f "$crash" ] || continue
  target=$(basename "$(dirname "$crash")")
  hash=$(sha256sum "$crash" | cut -c1-16)
  seed=fuzz/seeds/$target/crash-$hash
  [ -e "$seed" ] && continue
  mkdir -p "$(dirname "$seed")"
  cp "$crash" "$seed"
  found+=("$seed")
done

if [ ${#found[@]} -eq 0 ]; then
  echo "No new crashing inputs."
  exit 0
fi

git add "${found[@]}"
git commit --quiet -m "Add the inputs that crashed the fuzzers in run $run as seeds"
git push --quiet origin "$branch"
gh pr create --draft --base main --head "$branch" \
  --title "Fuzzers crashed on ${#found[@]} new input(s)" \
  --body "Run $GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$run saved:

$(printf -- '- `%s`\n' "${found[@]}")

Each is a reproducer. Merge this together with the fix, since a seed fails its target until the bug is fixed."
