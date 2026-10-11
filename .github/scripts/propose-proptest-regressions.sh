#!/usr/bin/env bash
# Adds the counterexamples (cc lines) the property runs saved to the
# standing ci/proptest branch and keeps one draft PR from it to main open.
# The branch is never force-pushed: each run merges main into it first.
# Usage: propose-proptest-regressions.sh <directory the property artifacts were downloaded to>
set -euo pipefail

artifacts=$(realpath "${1:?artifact directory}")
run=${GITHUB_RUN_ID:?}

git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
git fetch --quiet origin main
if git fetch --quiet origin ci/proptest 2>/dev/null; then
  git switch --quiet -C ci/proptest FETCH_HEAD
  git merge --quiet --no-edit origin/main
else
  git switch --quiet -c ci/proptest origin/main
fi

changed=()
while IFS= read -r saved; do
  repo=${saved#"$artifacts"/*/}
  if [ ! -e "$repo" ]; then
    mkdir -p "$(dirname "$repo")"
    cp "$saved" "$repo"
    changed+=("$repo")
    continue
  fi
  added=0
  while IFS= read -r line; do
    grep -qxF -- "$line" "$repo" && continue
    echo "$line" >>"$repo"
    added=1
  done < <(grep '^cc ' "$saved" || true)
  [ $added -eq 1 ] && changed+=("$repo")
done < <(find "$artifacts" -path '*/proptest-regressions/*' -name '*.txt' -type f)

if [ ${#changed[@]} -eq 0 ]; then
  echo "No new counterexamples."
  exit 0
fi

git add "${changed[@]}"
git commit --quiet -m "Add counterexamples found by property run $run"
git push --quiet origin ci/proptest

if [ -z "$(gh pr list --head ci/proptest --state open --json number --jq '.[].number')" ]; then
  gh pr create --draft --base main --head ci/proptest \
    --title "Property tests found counterexamples" \
    --body "Counterexamples saved by the scheduled property runs, as proptest regression lines. Merge together with the fix: each line replays first and fails until the bug is fixed. This branch collects later runs' counterexamples until the PR is merged."
fi
