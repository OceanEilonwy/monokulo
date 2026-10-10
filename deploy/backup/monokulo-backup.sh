#!/usr/bin/env bash
# Online backup of a running Monokulo's databases: monokulo.db and the
# engine's engine.db, whichever of the two are in its data folder (both, with
# the engine inside monokulo; one, when the engine runs on its own host).
#
# USAGE:
#   deploy/backup/monokulo-backup.sh <data-directory> <backup-directory> [--retain-days N]
#
# Writes <backup-directory>/monokulo-<UTC timestamp>/ holding a snapshot of
# each database and SHA256SUMS (so a copy to off-box storage can be checked
# afterwards), and prints what each snapshot holds, so an operator or a cron
# log has something concrete to look at on every run, not just an exit code.
# The data folder is ~/.local/share/monokulo by default, /srv/monokulo on
# OpenWrt and /var/lib/monokulo in the Docker image (docs/RUNNING.md).
#
# What a restore also needs, and this does not copy: the encryption key
# (MONOKULO_ENCRYPTION_KEY, which encrypts monokulo's data at rest) and the
# options file. Keep both wherever you keep these backups.
#
# WHY NOT `cp monokulo.db somewhere`:
#   Both databases run in WAL mode, so each is really two files at any moment
#   (the .db and a -wal holding committed frames not yet checkpointed), and a
#   scan round or a new order can write at any second. A plain copy taken
#   mid-commit can land between two pages of one transaction: not a state the
#   database was ever in. Copying the .db, -wal and -shm doesn't fix it either,
#   since nothing makes three copies atomic with each other.
#
# WHY `sqlite3 .backup`, not `VACUUM INTO`:
#   Both use SQLite's online-backup machinery and both make a consistent
#   snapshot when they succeed; rehearsals against a WAL database under a busy
#   writer saw neither fail. `.backup` is SQLite's purpose-built mechanism (the
#   Online Backup API), and it copies page by page in small steps, retrying on
#   SQLITE_BUSY, where `VACUUM INTO` holds one read transaction for its whole
#   run, which on a large busy database stops checkpoints and lets the -wal
#   file grow until it finishes. crates/engine/tests/backup_restore.rs runs
#   this script against a real engine database while a writer keeps inserting.
#
#   Each snapshot is complete on its own (no -wal or -shm needed) and
#   consistent at its own instant. The two are taken one after the other, a
#   moment apart: anything either references in the other (a store's engine
#   tenant) was created before the backup began, so the pair restores
#   together.
#
# SCHEDULING: monokulo-backup.service and monokulo-backup.timer beside this
# script are a ready-to-install systemd timer. Under plain cron, guard the
# line with flock, since cron will stack runs that overlap:
#
#   17 3 * * * flock -n /run/lock/monokulo-backup.lock \
#       /opt/monokulo/deploy/backup/monokulo-backup.sh /var/lib/monokulo \
#       /var/backups/monokulo --retain-days 14 >> /var/log/monokulo-backup.log 2>&1
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 <data-directory> <backup-directory> [--retain-days N]" >&2
    exit 2
fi

DATA_DIR="$1"
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
    echo "error: sqlite3 CLI not found on PATH - this script backs up with its .backup command" >&2
    exit 1
fi

DATABASES=()
for name in monokulo.db engine.db; do
    if [[ -f "$DATA_DIR/$name" ]]; then DATABASES+=("$name"); fi
done
if [[ ${#DATABASES[@]} -eq 0 ]]; then
    echo "error: neither monokulo.db nor engine.db is in $DATA_DIR" >&2
    exit 1
fi

mkdir -p "$DEST_DIR"

# Two runs against the same backup directory never race each other's
# in-progress folder. (The cron-level flock above stops scheduled runs piling
# up; this covers a run started by hand at the same time.)
LOCK_FILE="$DEST_DIR/.backup.lock"
LOCK_DIR=""
busy() {
    echo "error: another backup into $DEST_DIR appears to be running (lock: $LOCK_FILE)" >&2
    exit 1
}
release_lock() { if [[ -n "$LOCK_DIR" ]]; then rm -rf "$LOCK_DIR"; fi; }
if command -v flock >/dev/null 2>&1; then
    exec 9>"$LOCK_FILE"
    flock -n 9 || busy
else
    # macOS has no flock. mkdir is atomic, and the pid inside lets a lock
    # left behind by a killed run be taken over instead of blocking forever.
    if ! mkdir "$LOCK_FILE.d" 2>/dev/null; then
        holder="$(cat "$LOCK_FILE.d/pid" 2>/dev/null || true)"
        if [[ -n "$holder" ]] && kill -0 "$holder" 2>/dev/null; then busy; fi
        rm -rf "$LOCK_FILE.d"
        mkdir "$LOCK_FILE.d" 2>/dev/null || busy
    fi
    LOCK_DIR="$LOCK_FILE.d"
    echo $$ > "$LOCK_DIR/pid"
fi
trap release_lock EXIT

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DEST="$DEST_DIR/monokulo-${TIMESTAMP}"
PARTIAL="${DEST}.partial"
cleanup() { rm -rf "$PARTIAL"; release_lock; }
trap cleanup EXIT
mkdir -p "$PARTIAL"

for name in "${DATABASES[@]}"; do
    echo "==> backing up $DATA_DIR/$name (sqlite3 $(sqlite3 -version | cut -d' ' -f1))"
    sqlite3 "$DATA_DIR/$name" ".backup '$PARTIAL/$name'"
    INTEGRITY="$(sqlite3 "$PARTIAL/$name" "PRAGMA integrity_check;")"
    if [[ "$INTEGRITY" != "ok" ]]; then
        echo "error: PRAGMA integrity_check on the backup of $name did not return 'ok':" >&2
        echo "$INTEGRITY" >&2
        exit 1
    fi
done

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$PARTIAL" && sha256sum "${DATABASES[@]}" > SHA256SUMS)
elif command -v shasum >/dev/null 2>&1; then
    # macOS: same output format, and sha256sum -c reads it.
    (cd "$PARTIAL" && shasum -a 256 "${DATABASES[@]}" > SHA256SUMS)
fi

# Renamed into place only once complete and checked: the backup directory
# never holds a half-written backup under its final name.
mv "$PARTIAL" "$DEST"
trap release_lock EXIT

echo "==> backup ok: $DEST"
count() { sqlite3 "$1" "SELECT COUNT(*) FROM $2;" 2>/dev/null || echo "?"; }
for name in "${DATABASES[@]}"; do
    case "$name" in
        monokulo.db) echo "    monokulo.db: users=$(count "$DEST/$name" users) wallets=$(count "$DEST/$name" wallets) integrity_check=ok" ;;
        engine.db) echo "    engine.db: tenants=$(count "$DEST/$name" tenants) orders=$(count "$DEST/$name" orders) integrity_check=ok" ;;
    esac
done
echo "    keep the encryption key and the options file too: a restore needs both"

if [[ -n "$RETAIN_DAYS" ]]; then
    echo "==> removing backups older than $RETAIN_DAYS day(s) from $DEST_DIR"
    find "$DEST_DIR" -mindepth 1 -maxdepth 1 -type d -name 'monokulo-*' ! -name '*.partial' \
        -mtime "+$RETAIN_DAYS" -print -exec rm -rf {} +
fi
