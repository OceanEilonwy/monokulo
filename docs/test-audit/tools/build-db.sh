#!/usr/bin/env bash
# Rebuild audit.sqlite from the fresh JUnit reports.
set -euo pipefail
cd /home/henry/Downloads/mokulo/.claude/worktrees/test-audit
db=target/test-audit/audit.sqlite
t=target/test-audit/tools
rm -f "$db" "$db-wal" "$db-shm"
sqlite3 "$db" < "$t/schema.sql"
python3 -I "$t/load.py" "$db" rust target/coverage/rust/junit.xml
python3 -I "$t/load.py" "$db" rust target/test-audit/junit/xtask.xml
python3 -I "$t/load.py" "$db" browser target/coverage/browser/junit.xml "e2e:"
python3 -I "$t/load.py" "$db" ui-browser target/test-audit/junit/ui-browser.xml "ui-browser:"
python3 -I "$t/load.py" "$db" node target/test-audit/junit/apps-engine.xml "node:apps/engine:"
python3 -I "$t/load.py" "$db" node target/test-audit/junit/ui-lint.xml "node:crates/ui/tools:"
sqlite3 -header -column "$db" "select case when suite like 'e2e:%' then substr(suite,1,instr(substr(suite,5),':')+3) when instr(suite,'::') > 0 then substr(suite,1,instr(suite,'::')-1) else suite end grp, test_type, count(*) n, round(sum(duration_ms)/1000,1) secs, sum(failed) failed from test group by 1,2 order by secs desc"
