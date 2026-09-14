#!/usr/bin/env bash
# WBS 2.3.1: restore a moneropay-core backup (produced by backup-database.sh)
# onto a fresh box.
#
# ============================================================================
# THE REAL PROCEDURE - read this section first, under real pressure, before
# running anything. It assumes the scenario this step's outcome text names:
# "a backup taken from the running instance restores cleanly on a fresh box."
# ============================================================================
#
#   1. Get a moneropay-core binary and its config (moneropay.toml) onto the
#      new box - same version that produced the backup, or newer (migrations
#      in migrations/*.sql are forward-only and additive; an older binary
#      than the one that wrote the backup may refuse to start against a
#      schema_migrations row it doesn't recognize - see
#      shared::migrations::apply - so prefer "same version or newer," never
#      older).
#   2. Get the *latest* backup file onto the new box, from wherever it was
#      shipped off-box to (this script does not do that transfer itself -
#      it operates on a backup file already present locally; rsync/scp/your
#      object-storage CLI of choice is the right tool for the transfer leg,
#      and the .sha256 file backup-database.sh writes alongside each backup
#      is exactly what you diff after that transfer to confirm nothing
#      corrupted in transit).
#   3. Do NOT start moneropay-core against the destination path yet. Run this
#      script to place the backup at the config's expected database path
#      first:
#
#        scripts/restore-database.sh <backup-file> <destination-db-path>
#
#      e.g. scripts/restore-database.sh \
#             /var/backups/moneropay/moneropay-20260101T030000Z.db \
#             /var/lib/moneropay/moneropay.db
#
#      docs/DESIGN.md §4.1 is the reason the destination path matters: the
#      database always lives next to whichever config file is actually used
#      (moneropay.db in the config's own directory), so "the destination
#      path" here should be read straight off the new box's own
#      moneropay.toml, not assumed to match the old box's layout.
#   4. This script verifies the restored file with PRAGMA integrity_check and
#      PRAGMA foreign_key_check, and prints tenant/order counts - compare
#      those against what the *source* box (or backup-database.sh's own log
#      line from when this backup was taken) reported. This is the literal
#      acceptance bar this WBS step names: diffing tenant/order counts
#      before and after is the pass condition, not merely "the restore
#      command exited 0."
#   5. Only now start moneropay-core against the new box's config. On boot it
#      re-applies (already-applied, so harmless - see src/store.rs's own
#      comment on why migrations are re-run and re-checked every boot rather
#      than assumed-already-done) migrations, unseals every non-disabled
#      tenant's key material into a fresh in-memory KeyCustody registry
#      (PlainKeyCustody's registry does not itself survive a restart - see
#      docs/DESIGN.md §7.3), and rebuilds its scan watchlist from
#      `orders WHERE status NOT IN (...)` read fresh out of the restored
#      database. There is no separate "resume scanning" step to perform by
#      hand - a normal boot against a restored database *is* the resume.
#   6. Confirm the scanner is actually alive against the new box (not just
#      that the process started): the engine's own logs on a scan tick, or a
#      GET against a still-pending order's status endpoint, are the real
#      signal - a process that's up but never ticks is not actually
#      resumed.
#   7. Point DNS / your reverse proxy / merchants' registered webhook
#      expectations at the new box once you're satisfied, and decommission
#      the old one.
#
# What you do NOT need to do, and why: reconstruct -wal/-shm files (the
# backup is already a complete, checkpointed snapshot - see
# backup-database.sh's own header for why), re-derive any key material by
# hand (it's already sealed inside the restored tenants.sealed_key_material
# column exactly as KeyCustody::seal produced it - see docs/DESIGN.md §6.2),
# or manually recreate tenants/orders/webhooks (they're rows in the restored
# file, not external state to rebuild).
#
# ============================================================================
#
# USAGE:
#   scripts/restore-database.sh <backup-file> <destination-db-path> [--force]
#
# Refuses to overwrite an existing file at <destination-db-path> unless
# --force is given - a restore run against the wrong destination path by
# mistake should fail loudly, not silently clobber a database that might
# itself be live.
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <backup-file> <destination-db-path> [--force]" >&2
    exit 2
fi

BACKUP_FILE="$1"
DEST_PATH="$2"
shift 2

FORCE=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --force)
            FORCE=1
            shift
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "error: sqlite3 CLI not found on PATH - needed to verify the restored file" >&2
    exit 1
fi

if [[ ! -f "$BACKUP_FILE" ]]; then
    echo "error: backup file not found: $BACKUP_FILE" >&2
    exit 1
fi

# Verify the checksum sidecar if backup-database.sh left one next to this
# backup - catches transfer corruption before it's ever written to the
# destination, not after.
CHECKSUM_FILE="${BACKUP_FILE}.sha256"
if [[ -f "$CHECKSUM_FILE" ]] && command -v sha256sum >/dev/null 2>&1; then
    echo "==> verifying checksum against $CHECKSUM_FILE"
    if ! (cd "$(dirname "$BACKUP_FILE")" && sha256sum -c "$(basename "$CHECKSUM_FILE")") >/dev/null; then
        echo "error: checksum mismatch - $BACKUP_FILE does not match $CHECKSUM_FILE (corrupted in transit?)" >&2
        exit 1
    fi
    echo "    checksum ok"
else
    echo "==> no checksum sidecar found next to $BACKUP_FILE - skipping (not fatal, but verify provenance some other way)"
fi

echo "==> checking the backup file is itself a valid, consistent SQLite database"
BACKUP_INTEGRITY="$(sqlite3 "$BACKUP_FILE" "PRAGMA integrity_check;")"
if [[ "$BACKUP_INTEGRITY" != "ok" ]]; then
    echo "error: PRAGMA integrity_check on $BACKUP_FILE did not return 'ok':" >&2
    echo "$BACKUP_INTEGRITY" >&2
    exit 1
fi

if [[ -e "$DEST_PATH" && "$FORCE" -ne 1 ]]; then
    echo "error: $DEST_PATH already exists - refusing to overwrite without --force" >&2
    echo "       (if an engine process has this file open, stop it first regardless of --force -" >&2
    echo "       overwriting a database out from under a running writer is its own real hazard,"  >&2
    echo "       independent of anything this script checks)" >&2
    exit 1
fi

mkdir -p "$(dirname "$DEST_PATH")"

# Plain `cp`, deliberately - unlike backing up a *live* database, the backup
# file is already a static, checkpointed snapshot nothing is writing to, so
# none of backup-database.sh's online-backup reasoning applies here. Still
# staged through a temp name + atomic rename, exactly like backup-database.sh,
# so a restore killed partway through never leaves a half-written file sitting
# under the real destination path for something to accidentally start against.
TMP_DEST="${DEST_PATH}.restoring"
cleanup() { rm -f "$TMP_DEST"; }
trap cleanup EXIT
cp "$BACKUP_FILE" "$TMP_DEST"
mv "$TMP_DEST" "$DEST_PATH"
trap - EXIT

echo "==> verifying restored database at $DEST_PATH"
INTEGRITY="$(sqlite3 "$DEST_PATH" "PRAGMA integrity_check;")"
FK_CHECK="$(sqlite3 "$DEST_PATH" "PRAGMA foreign_key_check;")"
if [[ "$INTEGRITY" != "ok" ]]; then
    echo "error: PRAGMA integrity_check on the restored file did not return 'ok':" >&2
    echo "$INTEGRITY" >&2
    exit 1
fi
if [[ -n "$FK_CHECK" ]]; then
    echo "error: PRAGMA foreign_key_check on the restored file found violations:" >&2
    echo "$FK_CHECK" >&2
    exit 1
fi

TENANT_COUNT="$(sqlite3 "$DEST_PATH" "SELECT COUNT(*) FROM tenants;")"
ORDER_COUNT="$(sqlite3 "$DEST_PATH" "SELECT COUNT(*) FROM orders;")"
PAYMENT_COUNT="$(sqlite3 "$DEST_PATH" "SELECT COUNT(*) FROM order_payments;")"
echo "==> restore ok: $DEST_PATH"
echo "    tenants=$TENANT_COUNT orders=$ORDER_COUNT order_payments=$PAYMENT_COUNT integrity_check=ok foreign_key_check=ok"
echo "==> compare these counts against the source instance before trusting this restore -"
echo "    that comparison, not this script's exit code alone, is the actual pass condition."
