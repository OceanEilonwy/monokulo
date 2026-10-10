#!/usr/bin/env bash
# Restores a backup made by monokulo-backup.sh into a data folder, on the same
# box or a fresh one.
#
# USAGE:
#   deploy/backup/monokulo-restore.sh <backup-folder> <data-directory> [--force]
#
# e.g. deploy/backup/monokulo-restore.sh \
#        /var/backups/monokulo/monokulo-20260101T030000Z /var/lib/monokulo
#
# THE PROCEDURE - read this first:
#   1. Put monokulo on the box: the version that made the backup or a newer one
#      (migrations are forward-only, and an older binary may refuse a schema it
#      doesn't know). Bring the options file and the SAME encryption key
#      (MONOKULO_ENCRYPTION_KEY): monokulo's data is encrypted at rest with it,
#      and nothing in the backup can be read without it.
#   2. Copy the backup folder onto the box. SHA256SUMS inside it is what this
#      script checks after the transfer.
#   3. Make sure monokulo (and a separate engine, if you run one) is stopped,
#      then run this script into the data folder the options file names
#      (the default is ~/.local/share/monokulo; /srv/monokulo on OpenWrt;
#      /var/lib/monokulo in the Docker image).
#   4. Compare the counts it prints with the ones monokulo-backup.sh printed
#      when it made the backup. Matching counts are the pass condition, not
#      just this script's exit code.
#   5. Start monokulo. It re-applies its migrations (already applied, so
#      harmless), unseals each store's keys and rebuilds its scan list from the
#      restored orders: a normal start against restored databases is the
#      resume. Check it is really scanning (the engine's log lines on a scan
#      round, or a pending order's status) before pointing DNS, your reverse
#      proxy or merchants at it.
#
# Refuses to overwrite a database already in the data folder unless --force
# is given, and checks every file before writing any of them.
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <backup-folder> <data-directory> [--force]" >&2
    exit 2
fi

BACKUP="$1"
DATA_DIR="$2"
shift 2

FORCE=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --force) FORCE=1; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "error: sqlite3 CLI not found on PATH - needed to check the restored databases" >&2
    exit 1
fi
if [[ ! -d "$BACKUP" ]]; then
    echo "error: backup folder not found: $BACKUP" >&2
    exit 1
fi

DATABASES=()
for name in monokulo.db engine.db; do
    if [[ -f "$BACKUP/$name" ]]; then DATABASES+=("$name"); fi
done
if [[ ${#DATABASES[@]} -eq 0 ]]; then
    echo "error: $BACKUP holds neither monokulo.db nor engine.db" >&2
    exit 1
fi

# Transfer corruption is caught before anything is written.
SHA256_CHECK=""
if command -v sha256sum >/dev/null 2>&1; then SHA256_CHECK="sha256sum -c"
elif command -v shasum >/dev/null 2>&1; then SHA256_CHECK="shasum -a 256 -c"  # macOS
fi
if [[ -f "$BACKUP/SHA256SUMS" ]] && [[ -n "$SHA256_CHECK" ]]; then
    echo "==> checking $BACKUP/SHA256SUMS"
    if ! (cd "$BACKUP" && $SHA256_CHECK SHA256SUMS) >/dev/null; then
        echo "error: checksum mismatch in $BACKUP (corrupted in transit?)" >&2
        exit 1
    fi
    echo "    checksums ok"
else
    echo "==> no SHA256SUMS in $BACKUP - skipping (not fatal, but check where it came from)"
fi

for name in "${DATABASES[@]}"; do
    INTEGRITY="$(sqlite3 "$BACKUP/$name" "PRAGMA integrity_check;")"
    if [[ "$INTEGRITY" != "ok" ]]; then
        echo "error: PRAGMA integrity_check on $BACKUP/$name did not return 'ok':" >&2
        echo "$INTEGRITY" >&2
        exit 1
    fi
    if [[ -e "$DATA_DIR/$name" && "$FORCE" -ne 1 ]]; then
        echo "error: $DATA_DIR/$name already exists - refusing to overwrite without --force" >&2
        echo "       (stop monokulo first in any case: replacing a database under a running" >&2
        echo "       writer is its own hazard, whatever this script checks)" >&2
        exit 1
    fi
done

mkdir -p "$DATA_DIR"
for name in "${DATABASES[@]}"; do
    # A plain copy is right here: the backup is a static snapshot nothing
    # writes to. Through a temporary name, so a restore stopped part way
    # never leaves half a database under the real name. A stale -wal or -shm
    # beside the old database would be applied to the new one: removed.
    rm -f "$DATA_DIR/$name-wal" "$DATA_DIR/$name-shm"
    cp "$BACKUP/$name" "$DATA_DIR/$name.restoring"
    mv "$DATA_DIR/$name.restoring" "$DATA_DIR/$name"
done

echo "==> restored into $DATA_DIR"
count() { sqlite3 "$1" "SELECT COUNT(*) FROM $2;" 2>/dev/null || echo "?"; }
for name in "${DATABASES[@]}"; do
    INTEGRITY="$(sqlite3 "$DATA_DIR/$name" "PRAGMA integrity_check;")"
    FK_CHECK="$(sqlite3 "$DATA_DIR/$name" "PRAGMA foreign_key_check;")"
    if [[ "$INTEGRITY" != "ok" || -n "$FK_CHECK" ]]; then
        echo "error: the restored $name failed its checks:" >&2
        echo "$INTEGRITY" >&2
        echo "$FK_CHECK" >&2
        exit 1
    fi
    case "$name" in
        monokulo.db) echo "    monokulo.db: users=$(count "$DATA_DIR/$name" users) wallets=$(count "$DATA_DIR/$name" wallets) integrity_check=ok foreign_key_check=ok" ;;
        engine.db) echo "    engine.db: tenants=$(count "$DATA_DIR/$name" tenants) orders=$(count "$DATA_DIR/$name" orders) order_payments=$(count "$DATA_DIR/$name" order_payments) integrity_check=ok foreign_key_check=ok" ;;
    esac
done
echo "==> compare these counts with the ones the backup printed before trusting this restore"
