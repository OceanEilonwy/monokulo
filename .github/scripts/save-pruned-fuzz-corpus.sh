#!/usr/bin/env bash
# Removes from the ci/fuzz branch the inputs the prune jobs found redundant:
# each job restored corpus/<target> (its names in fuzz/corpus-restored/<target>)
# and minimized it with `cargo fuzz cmin`, so a restored name missing from its
# minimized corpus adds no coverage the rest doesn't. Inputs added since the
# job restored stay.
# Usage: save-pruned-fuzz-corpus.sh <directory the prune artifacts were downloaded to>
set -euo pipefail

artifacts=$(realpath "${1:?artifact directory}")
work=$(mktemp -d)

git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'

# A push can race a fuzz run's save; a rejected one starts over from the new tip.
for attempt in 1 2 3 4 5; do
  rm -rf "$work/tree"
  git worktree prune
  git fetch --quiet origin ci/fuzz
  git worktree add --quiet --force -B ci/fuzz "$work/tree" FETCH_HEAD

  summary=""
  for restored in "$artifacts"/*/fuzz/corpus-restored/*; do
    [ -f "$restored" ] || continue
    target=$(basename "$restored")
    kept="$(dirname "$(dirname "$restored")")/corpus/$target"
    # A job whose minimized corpus is missing failed before it finished.
    [ -d "$kept" ] || continue
    removed=0
    while IFS= read -r name; do
      [ -n "$name" ] && [ ! -e "$kept/$name" ] && [ -f "$work/tree/corpus/$target/$name" ] || continue
      git -C "$work/tree" rm --quiet "corpus/$target/$name"
      removed=$((removed + 1))
    done < "$restored"
    summary+="$target: removed $removed of $(wc -l < "$restored")"$'\n'
  done
  printf '%s' "$summary"

  if git -C "$work/tree" diff --cached --quiet; then
    echo "Nothing to prune."
    exit 0
  fi
  git -C "$work/tree" commit --quiet -m "Prune the corpus (run ${GITHUB_RUN_ID:-local})" -m "$summary"
  if git -C "$work/tree" push --quiet origin HEAD:ci/fuzz; then
    echo "Pruned the corpus on ci/fuzz."
    exit 0
  fi
  echo "Push rejected (attempt $attempt); retrying from the new tip."
done
echo "Could not push to ci/fuzz." >&2
exit 1
