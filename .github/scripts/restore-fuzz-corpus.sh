#!/usr/bin/env bash
# Seeds fuzz/corpus/<target> from the ci/fuzz branch, so a campaign starts
# from the saved corpus. The names restored go in fuzz/corpus-restored/<target>,
# so saving adds back only what the run found: an input pruned from ci/fuzz
# meanwhile stays pruned.
# Usage: restore-fuzz-corpus.sh <target>
set -euo pipefail

target=${1:?target}

mkdir -p fuzz/corpus-restored
: > "fuzz/corpus-restored/$target"
if ! git fetch --quiet --depth=1 origin ci/fuzz 2>/dev/null; then
  echo "No ci/fuzz branch yet; starting from the reviewed seeds."
  exit 0
fi
if git cat-file -e "FETCH_HEAD:corpus/$target" 2>/dev/null; then
  git archive FETCH_HEAD "corpus/$target" | tar -x -C fuzz
  git ls-tree --name-only FETCH_HEAD "corpus/$target/" | xargs -r -n1 basename \
    > "fuzz/corpus-restored/$target"
  echo "Restored $(wc -l < "fuzz/corpus-restored/$target") inputs from ci/fuzz."
fi
