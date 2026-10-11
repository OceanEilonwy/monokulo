#!/usr/bin/env bash
# Seeds fuzz/corpus/<target> from the ci/fuzz branch, so a campaign starts
# from the saved corpus when the Actions cache has been evicted.
# Usage: restore-fuzz-corpus.sh <target>
set -euo pipefail

target=${1:?target}

if ! git fetch --quiet --depth=1 origin ci/fuzz 2>/dev/null; then
  echo "No ci/fuzz branch yet; starting from the reviewed seeds."
  exit 0
fi
mkdir -p fuzz
if git cat-file -e "FETCH_HEAD:corpus/$target" 2>/dev/null; then
  git archive FETCH_HEAD "corpus/$target" | tar -x -C fuzz
  echo "Restored $(find "fuzz/corpus/$target" -type f | wc -l) inputs from ci/fuzz."
fi
