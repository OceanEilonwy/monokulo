#!/usr/bin/env bash
# Adds the corpus inputs the fuzz jobs found to the ci/fuzz branch, an
# orphan branch that holds only corpus/<target>/<input>. libFuzzer names an
# input by the hash of its bytes and keeps only inputs that added coverage,
# so adding the new names is a union. The reviewed seeds are in main already,
# and the inputs a job restored (fuzz/corpus-restored/<target>) aren't added
# back, so an input pruned from ci/fuzz since then stays pruned.
# Usage: save-fuzz-corpus.sh <directory the fuzz artifacts were downloaded to>
set -euo pipefail

artifacts=$(realpath "${1:?artifact directory}")
work=$(mktemp -d)

git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'

# A push can race another run; a rejected one starts over from the new tip.
for attempt in 1 2 3 4 5; do
  rm -rf "$work/tree"
  git worktree prune
  if git fetch --quiet origin ci/fuzz 2>/dev/null; then
    git worktree add --quiet --force -B ci/fuzz "$work/tree" FETCH_HEAD
  else
    git worktree add --quiet --force --orphan -b ci/fuzz "$work/tree"
  fi

  for corpus in "$artifacts"/*/fuzz/corpus/*/; do
    [ -d "$corpus" ] || continue
    target=$(basename "$corpus")
    restored="$corpus../../corpus-restored/$target"
    mkdir -p "$work/tree/corpus/$target"
    for input in "$corpus"*; do
      [ -f "$input" ] || continue
      name=$(basename "$input")
      case $name in reviewed-*) continue ;; esac
      if [ -f "$restored" ] && grep -qxF "$name" "$restored"; then continue; fi
      cp -n "$input" "$work/tree/corpus/$target/"
    done
  done

  git -C "$work/tree" add --all
  if git -C "$work/tree" diff --cached --quiet; then
    echo "The corpus found nothing new."
    exit 0
  fi
  git -C "$work/tree" commit --quiet -m "Add corpus inputs found by run ${GITHUB_RUN_ID:-local}"
  if git -C "$work/tree" push --quiet origin HEAD:ci/fuzz; then
    echo "Saved the new corpus inputs to ci/fuzz."
    exit 0
  fi
  echo "Push rejected (attempt $attempt); retrying from the new tip."
done
echo "Could not push to ci/fuzz." >&2
exit 1
