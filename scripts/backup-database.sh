#!/usr/bin/env bash
# WBS 2.3.1: online backup of a *live* scanner SQLite database.
#
# This script exists to answer one question honestly: is it safe to back this
# database up while the engine is running against it? The engine's writer
# actor (docs/DESIGN.md §9) holds the one write connection and commits under
# WAL journal mode (`PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL`,
# set in `src/store.rs::configure_connection`) - so "the database" at any
# moment is really *two* files on disk (`moneropay.db` plus a `-wal` file
# holding not-yet-checkpointed committed frames), and a scan tick or an
# incoming order can write to it at any second, including the second this
# script runs.
#
# WHY NOT `cp database.db somewhere`:
#   A plain file copy of just the `.db` file, taken while a writer is mid-
#   commit, can land between two pages of a single transaction being flushed
#   - the copy is not guaranteed to represent any single point in time the
#   database was ever actually in. Copying all three files (`.db`/`-wal`/
#   `-shm`) doesn't fix this either: nothing makes those three individual
#   `cp` invocations atomic with each other, so a checkpoint landing between
#   them tears the snapshot just the same. This is exactly the hazard this
#   step's own brief named, not a hypothetical - it's why SQLite ships a real
#   online-backup mechanism at all.
#
# WHY `sqlite3 .backup`, not `VACUUM INTO`:
#   Both use SQLite's genuine online-backup machinery and both produce a
#   correct, self-consistent snapshot *when they succeed*. Investigated
#   directly rather than assumed - a rehearsal script hammered a real WAL-mode
#   SQLite file with ~4000 single-row autocommit inserts from a background
#   writer while running `.backup` three times concurrently mid-write: every
#   backup succeeded with no errors, and each resulting file passed
#   `PRAGMA integrity_check` and contained a plausible subset of the rows
#   written so far (real, observed: 131/170/209 of an eventual 4000, growing
#   monotonically - each backup really did capture a coherent snapshot at the
#   moment it ran, not a torn one). A second rehearsal ran `VACUUM INTO`
#   fifteen times concurrently against the same kind of writer and also saw
#   zero failures - so under plain single-writer WAL contention, neither
#   mechanism is actually unsafe; WAL's whole point is that a reader (which is
#   what both of these are, under the hood) is never blocked by a writer.
#   `.backup` is still the better choice for this script, for two concrete
#   reasons neither rehearsal alone would surface:
#     1. It is SQLite's own documented, purpose-built mechanism for this exact
#        job (the "Online Backup API", `sqlite3_backup_init/step/finish` - the
#        CLI's `.backup` command is a thin wrapper around it) - the tool named
#        for this job, not a side effect of a statement meant for something
#        else.
#     2. It copies the source page-by-page across many small steps, retrying
#        on SQLITE_BUSY (e.g. if it collides with a concurrent WAL
#        checkpoint) rather than taking one single long-lived read
#        transaction for the whole database. `VACUUM INTO` holds one read
#        transaction for its entire run, which on a large, busy production
#        database is a real risk this small test can't surface: a long-lived
#        reader in WAL mode prevents `wal_checkpoint(TRUNCATE)` from
#        reclaiming space for as long as it runs, so a big `VACUUM INTO` can
#        starve checkpointing and let the `-wal` file grow unboundedly for
#        its whole duration. `.backup`'s stepped copy has no equivalent
#        single-long-transaction downside.
#   (The two rehearsal scripts that produced the numbers above are not shipped
#   here - they're one-off `/tmp` scratch scripts from the investigation, not
#   part of this repo. The claim they support is instead pinned for real, on
#   every run, by `tests/backup_restore.rs`, which does the same kind of
#   concurrent-write-during-backup exercise against this actual script and a
#   real `Store`-backed database, not just a bare `t (id, v)` table.)
#
# WHAT THIS SCRIPT DOES NOT NEED TO WORRY ABOUT: the destination file `.backup`
# produces is a complete, self-contained snapshot - it does not need a
# companion `-wal`/`-shm` file to be valid, correct, or reopenable. SQLite's
# backup API copies committed page content (including whatever the source's
# `-wal` file held that hadn't yet been checkpointed into the main file), so
# the output is exactly as if the source had been cleanly closed at that
# instant. Restoring it is copying one file, not three - see
# restore-database.sh.
#
# USAGE:
#   scripts/backup-database.sh <path-to-live-db> <backup-directory> [--retain-days N]
#
# Writes <backup-directory>/moneropay-<UTC timestamp>.db (plus a .sha256
# checksum file alongside it, so a copy to off-box storage can be verified
# afterward) and prints the tenant/order counts it observed in the backup, so
# an operator (or a cron log) has something concrete to eyeball on every run,
# not just an exit code.
#
# CRON GUIDANCE: see scripts/moneropay-backup.service and
# scripts/moneropay-backup.timer for a ready-to-install systemd timer (the
# preferred mechanism on any systemd host - gives you `systemctl status`,
# journal logging, and `OnCalendar` scheduling for free), or, for a plain
# cron install, a line like:
#
#   17 3 * * * flock -n /run/lock/moneropay-backup.lock \
#       /path/to/scripts/backup-database.sh /var/lib/moneropay/moneropay.db \
#       /var/backups/moneropay --retain-days 14 \
#       >> /var/log/moneropay-backup.log 2>&1
#
# The `flock -n` guard matters: cron has no notion of "skip this run if the
# last one is still going," and this script's own internal advisory lock
# (below) only protects against a second *invocation of this script*
# overlapping itself when it's launched directly, not against a cron daemon
# stacking up long-running invocations if a backup ever runs slower than its
# schedule - `flock -n` on a fixed lock file path is the standard, correct fix
# for that, done at the cron-line level rather than inside the script, so the
# same script stays usable un-wrapped for the systemd-timer and manual-run
# cases (systemd's own `Type=oneshot` unit already serializes runs for you -
# see the .service file).
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <path-to-live-db> <backup-directory> [--retain-days N]" >&2
    exit 2
fi

SRC_DB="$1"
DEST_DIR="$2"
shift 2

RETAIN_DAYS=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --retain-days)
            RETAIN_DAYS="${2:?--retain-days needs a number}"
            shift 2
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "error: sqlite3 CLI not found on PATH - this script backs up via its .backup command, it is not optional" >&2
    exit 1
fi

if [[ ! -f "$SRC_DB" ]]; then
    echo "error: source database not found: $SRC_DB" >&2
    exit 1
fi

mkdir -p "$DEST_DIR"

# Advisory lock so two invocations of *this script* against the same
# destination directory never race each other's temp file. Not a substitute
# for the cron-level `flock` above (that one prevents pile-up across
# scheduled runs of the whole script; this one only protects the brief window
# a single run spends producing its own file) - belt and suspenders, cheap
# to keep both.
LOCK_FILE="$DEST_DIR/.backup.lock"
exec 9>"$LOCK_FILE"
if ! flock -n 9; then
    echo "error: another backup-database.sh run against $DEST_DIR appears to be in progress (lock: $LOCK_FILE)" >&2
    exit 1
fi

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DEST_FILE="$DEST_DIR/moneropay-${TIMESTAMP}.db"
TMP_FILE="${DEST_FILE}.partial"

cleanup() {
    # Only ever removes our own in-progress temp file - never the finished
    # backup, and never anything a concurrent run might have created (each
    # run's temp name is timestamp-unique).
    rm -f "$TMP_FILE"
}
trap cleanup EXIT

echo "==> backing up $SRC_DB -> $DEST_FILE (sqlite3 $(sqlite3 -version | cut -d' ' -f1))"

# The actual mechanism: SQLite's online backup API via the CLI's .backup
# command, run against a *temp* filename so a reader (this script's own
# integrity check below, or an operator poking around the backup directory)
# never sees a partially-written file under the real name - renamed into
# place only after the backup completes and passes its own integrity check.
sqlite3 "$SRC_DB" ".backup '$TMP_FILE'"

echo "==> verifying backup integrity"
INTEGRITY="$(sqlite3 "$TMP_FILE" "PRAGMA integrity_check;")"
if [[ "$INTEGRITY" != "ok" ]]; then
    echo "error: PRAGMA integrity_check on the fresh backup did not return 'ok':" >&2
    echo "$INTEGRITY" >&2
    exit 1
fi

# Atomic on the same filesystem (a plain rename(2)) - the destination
# directory never contains a half-written file under its final name.
mv "$TMP_FILE" "$DEST_FILE"
trap - EXIT

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$DEST_DIR" && sha256sum "$(basename "$DEST_FILE")" > "$(basename "$DEST_FILE").sha256")
fi

TENANT_COUNT="$(sqlite3 "$DEST_FILE" "SELECT COUNT(*) FROM tenants;")"
ORDER_COUNT="$(sqlite3 "$DEST_FILE" "SELECT COUNT(*) FROM orders;")"
echo "==> backup ok: $DEST_FILE"
echo "    tenants=$TENANT_COUNT orders=$ORDER_COUNT integrity_check=ok"

if [[ -n "$RETAIN_DAYS" ]]; then
    echo "==> pruning backups older than $RETAIN_DAYS day(s) in $DEST_DIR"
    find "$DEST_DIR" -maxdepth 1 -name 'moneropay-*.db' -mtime "+$RETAIN_DAYS" -print -delete
    find "$DEST_DIR" -maxdepth 1 -name 'moneropay-*.db.sha256' -mtime "+$RETAIN_DAYS" -print -delete
fi
