#!/usr/bin/env bash
# Per-step durations (seconds) for every job of a run: ci_steps.sh <run-id>
set -euo pipefail
cd /home/henry/Downloads/mokulo
gh api "repos/{owner}/{repo}/actions/runs/$1/jobs?per_page=100" --paginate \
  -q '.jobs[] | select(.conclusion != "skipped") | .name as $j | .steps[] | select(.started_at != null and .completed_at != null) |
      [$j, .name, ((.completed_at|fromdate) - (.started_at|fromdate))] | @tsv' |
  awk -F'\t' '$3 >= 20'
