#!/usr/bin/env bash
# Start/stop the local Monokulo dev stack: the engine
# (scanner) and monokulo, running together the same way
# they do in production - two separate processes, monokulo
# talking to the engine over its own admin API.
#
# USAGE:
#   scripts/dev-run.sh start [--no-build]
#   scripts/dev-run.sh stop
#   scripts/dev-run.sh restart [--no-build]
#   scripts/dev-run.sh status
#   scripts/dev-run.sh logs [engine|monokulo]
#
# `start` builds both debug binaries first by default (skip with
# --no-build for a faster restart when you know nothing changed) and is
# safe to run again while already running - it just reports what's up,
# except that a process older than its binary is restarted, so a rebuild
# always takes effect.
#
# WHERE STATE LIVES: everything this script creates lives under
# .dev-run/ at the repo root (gitignored) - PID files, logs, each process's
# options file and SQLite database, and two locally-generated secrets
# (MONOKULO_ENCRYPTION_KEY and the engine token). Nothing here is checked
# in, and nothing here is a real credential worth protecting beyond your
# own machine - this is a local dev stack against a real *stagenet* wallet
# (worthless XMR only), the same wallet e2e/moneropay-stagenet.toml
# already uses for the repo's own real end-to-end test.
#
# SETTINGS. Each process reads its options file, passed with --options:
# .dev-run/engine/engine.toml and .dev-run/monokulo/monokulo.toml. This
# script writes each one the first time, with the dev values (the stagenet
# node, fast-iteration payment thresholds, the plain key custody backend,
# where each listens and keeps its database), and never again: after that
# they are yours, edited by hand or on the admin settings page. Delete one
# to get the dev values back. The secrets are never in them: the engine
# token goes to the engine as ENGINE_TOKEN and to monokulo as
# MONOKULO_ENGINE_TOKEN (the engine answers no request without it, and
# neither process starts without it), the encryption key as
# MONOKULO_ENCRYPTION_KEY.
#
# The engine listens on 127.0.0.1:8080 here, and monokulo's engine.url
# says so. (Left to their defaults, both sides agree on 127.0.0.1:8443
# instead.)
#
# monokulo's own admin account no longer needs seeding by this script at
# all: opening http://127.0.0.1:8081 for the first time now redirects
# straight to its own first-run admin setup wizard (create the one admin
# account right there in the browser) - see the "next steps" this script
# prints after `start`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR="$REPO_ROOT/.dev-run"
ENGINE_DIR="$RUN_DIR/engine"
CP_DIR="$RUN_DIR/monokulo"

ENGINE_BIN="$REPO_ROOT/target/debug/monokulo-engine"
CP_BIN="$REPO_ROOT/target/debug/monokulo"

ENGINE_OPTIONS="$ENGINE_DIR/engine.toml"
CP_OPTIONS="$CP_DIR/monokulo.toml"
ENGINE_TOKEN_FILE="$ENGINE_DIR/engine_token.txt"
CP_KEY_FILE="$CP_DIR/encryption_key.txt"

ENGINE_PID_FILE="$RUN_DIR/engine.pid"
CP_PID_FILE="$RUN_DIR/monokulo.pid"
ENGINE_LOG="$RUN_DIR/engine.log"
CP_LOG="$RUN_DIR/monokulo.log"

ENGINE_URL="http://127.0.0.1:8080"
CONTROL_PLANE_URL="http://127.0.0.1:8081"


usage() {
    cat <<'EOF'
usage: dev-run.sh <command> [options]

commands:
  start [--no-build]   build (unless --no-build) and start both processes
  stop                  stop both processes
  restart [--no-build]  stop, then start
  status                show whether each process is running
  logs [engine|monokulo]   tail logs (both by default)
EOF
}

is_running() {
    # $1: pid file
    [[ -f "$1" ]] && kill -0 "$(cat "$1")" 2>/dev/null
}

# Whether the process in pid file $1 started before binary $2 was last
# built - i.e. it's still running old code. `start` rebuilds, so without
# this a process left running from an earlier `start` would quietly keep
# serving the old build (and skip everything `start` does after launching
# it, such as provisioning engine settings). False if either time can't be
# read, so an unusual `ps`/`stat` never blocks `start`.
is_stale() {
    local pid elapsed built now
    pid="$(cat "$1")"
    elapsed="$(ps -o etimes= -p "$pid" 2>/dev/null | tr -d ' ')"
    built="$(stat -c %Y "$2" 2>/dev/null || stat -f %m "$2" 2>/dev/null)"
    [[ "$elapsed" =~ ^[0-9]+$ && "$built" =~ ^[0-9]+$ ]] || return 1
    now="$(date +%s)"
    (( now - elapsed < built ))
}

ensure_dirs() {
    mkdir -p "$ENGINE_DIR" "$CP_DIR"
}

ensure_engine_token() {
    if [[ -f "$ENGINE_TOKEN_FILE" ]]; then
        return
    fi
    if ! command -v openssl >/dev/null 2>&1; then
        echo "error: openssl not found - needed once, to generate a dev engine token" >&2
        exit 1
    fi
    echo "==> generating a dev engine token (persisted at $ENGINE_TOKEN_FILE, reused on every future start)"
    openssl rand -hex 32 > "$ENGINE_TOKEN_FILE"
    chmod 600 "$ENGINE_TOKEN_FILE"
}

ensure_cp_key() {
    if [[ -f "$CP_KEY_FILE" ]]; then
        return
    fi
    if ! command -v openssl >/dev/null 2>&1; then
        echo "error: openssl not found - needed once, to generate a dev MONOKULO_ENCRYPTION_KEY" >&2
        exit 1
    fi
    echo "==> generating a dev MONOKULO_ENCRYPTION_KEY (persisted at $CP_KEY_FILE, reused on every future start)"
    openssl rand -hex 32 > "$CP_KEY_FILE"
    chmod 600 "$CP_KEY_FILE"
}

build() {
    echo "==> building the engine and monokulo (debug)"
    (cd "$REPO_ROOT" && cargo build -p engine --bin monokulo-engine -p monokulo --bin monokulo)
}

# Writes each process's options file with the dev values, the first time
# only: after that it is the developer's, edited by hand or on the admin
# settings page. The stagenet node and payment thresholds are the ones
# e2e/moneropay-stagenet.toml and crates/engine/tests/support/mod.rs's
# e2e_fixture use. Key custody is per store; this enables only the
# in-process `plain` backend, the right choice for a dev stack with no
# key-custody-server.
ensure_options_files() {
    if [[ ! -f "$ENGINE_OPTIONS" ]]; then
        echo "==> writing the dev engine options file ($ENGINE_OPTIONS)"
        cat > "$ENGINE_OPTIONS" <<EOF
# The dev stack's engine settings, written once by scripts/dev-run.sh.
# Edit them here or on monokulo's admin settings page; delete this file
# to get the dev values back. \`monokulo-engine --init --options x.toml\`
# writes a file describing every setting.

[server]
bind = "127.0.0.1:8080"

[database]
path = "$ENGINE_DIR/engine.db"

[payment]
confirmations_required = 0
order_expiry_minutes = 30
reorg_check_depth = 20
mempool_poll_interval_ms = 2000

[key_custody]
enabled_backends = ["plain"]
default_backend = "plain"

[monero_node]
stagenet = { host = "node.monerodevs.org", port = 38089, ssl = false, accept_self_signed_certs = true, fallbacks = [{ host = "node2.monerodevs.org", port = 38089, ssl = false, accept_self_signed_certs = true, fallbacks = [] }] }
EOF
    fi
    if [[ ! -f "$CP_OPTIONS" ]]; then
        echo "==> writing the dev monokulo options file ($CP_OPTIONS)"
        cat > "$CP_OPTIONS" <<EOF
# The dev stack's monokulo settings, written once by scripts/dev-run.sh.
# Edit them here or on the admin settings page; delete this file to get
# the dev values back. \`monokulo --init --options x.toml\` writes a file
# describing every setting.

[server]
bind = "127.0.0.1:8081"

[database]
path = "$CP_DIR/monokulo.db"

[engine]
url = "$ENGINE_URL"
EOF
    fi
}

# Provisions the one self-hosted tenant, from the same watch-only stagenet
# wallet the repo's own e2e tests use - a one-time action (refuses once a
# tenant already exists), so this only ever does real work on the very
# first `start` against a fresh database. It reads the same options file
# as the engine, so the tenant starts with its confirmations_required and
# order_expiry_minutes (`local_admin::bootstrap_wallet`'s own doc comment).
ensure_wallet_bootstrapped() {
    local out
    # The view key goes in on standard input (printf is a shell builtin, so
    # it never shows in the process list), never as an argument.
    if out=$(printf '%s' "fcdc7998f003928b3f409b94d54f690d16ca6df3689de4da4803c5a9c792fb0e" \
        | "$ENGINE_BIN" --options "$ENGINE_OPTIONS" --bootstrap-wallet \
        --primary-address "54F1KdjaAtnL6Fb4SbLUM1AMQSjSERjYUgYRtVgwjBirA26RyJCzxc4TbWPW65ZvRC6bifBfrTTv3fyu25BFQuvA2ogNiXg" \
        --view-key-file - \
        --spend-pubkey "3fa2161d4e2cc7722288d33e46a4cc37e92629d7e45939ec67cc42e8f144b335" \
        --network stagenet 2>&1); then
        echo "==> bootstrapped the dev stagenet tenant:"
        echo "$out" | sed 's/^/    /'
    fi
    # A non-zero exit here just means "already bootstrapped" (a tenant
    # from a previous run's database) - not an error worth failing
    # `start` over, so deliberately not checked against `set -e`.
}

start_engine() {
    if is_running "$ENGINE_PID_FILE"; then
        if ! is_stale "$ENGINE_PID_FILE" "$ENGINE_BIN"; then
            echo "engine already running (pid $(cat "$ENGINE_PID_FILE"))"
            return
        fi
        echo "==> engine (pid $(cat "$ENGINE_PID_FILE")) is running an older build than $ENGINE_BIN - restarting it"
        stop_one "engine" "$ENGINE_PID_FILE"
    fi
    ensure_engine_token
    if [[ ! -x "$ENGINE_BIN" ]]; then
        echo "error: $ENGINE_BIN not found - run without --no-build at least once" >&2
        exit 1
    fi
    echo "==> starting engine -> $ENGINE_LOG"
    ENGINE_TOKEN="$(cat "$ENGINE_TOKEN_FILE")" \
        nohup "$ENGINE_BIN" --options "$ENGINE_OPTIONS" > "$ENGINE_LOG" 2>&1 &
    echo $! > "$ENGINE_PID_FILE"
    sleep 1
    if is_running "$ENGINE_PID_FILE"; then
        echo "    engine up (pid $(cat "$ENGINE_PID_FILE")) - $ENGINE_URL"
    else
        echo "    engine failed to start - see $ENGINE_LOG" >&2
        rm -f "$ENGINE_PID_FILE"
        exit 1
    fi
    ensure_wallet_bootstrapped
}

start_control_plane() {
    if is_running "$CP_PID_FILE"; then
        if ! is_stale "$CP_PID_FILE" "$CP_BIN"; then
            echo "monokulo already running (pid $(cat "$CP_PID_FILE"))"
            return
        fi
        echo "==> monokulo (pid $(cat "$CP_PID_FILE")) is running an older build than $CP_BIN - restarting it"
        stop_one "monokulo" "$CP_PID_FILE"
    fi
    ensure_cp_key
    if [[ ! -x "$CP_BIN" ]]; then
        echo "error: $CP_BIN not found - run without --no-build at least once" >&2
        exit 1
    fi
    echo "==> starting monokulo -> $CP_LOG"
    MONOKULO_ENCRYPTION_KEY="$(cat "$CP_KEY_FILE")" \
    MONOKULO_ENGINE_TOKEN="$(cat "$ENGINE_TOKEN_FILE")" \
        nohup "$CP_BIN" --options "$CP_OPTIONS" > "$CP_LOG" 2>&1 &
    echo $! > "$CP_PID_FILE"
    sleep 1
    if is_running "$CP_PID_FILE"; then
        echo "    monokulo up (pid $(cat "$CP_PID_FILE")) - $CONTROL_PLANE_URL"
    else
        echo "    monokulo failed to start - see $CP_LOG" >&2
        rm -f "$CP_PID_FILE"
        exit 1
    fi
}

stop_one() {
    # $1: display name, $2: pid file
    if ! is_running "$2"; then
        echo "$1 not running"
        rm -f "$2"
        return
    fi
    local pid
    pid="$(cat "$2")"
    echo "==> stopping $1 (pid $pid)"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 25); do
        is_running "$2" || break
        sleep 0.2
    done
    if is_running "$2"; then
        echo "    still running after 5s, sending SIGKILL"
        kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$2"
}

status_one() {
    # $1: display name, $2: pid file, $3: url
    if is_running "$2"; then
        echo "$1: running (pid $(cat "$2")) - $3"
    else
        echo "$1: stopped"
    fi
}

cmd="${1:-}"
case "$cmd" in
    start)
        ensure_dirs
        ensure_options_files
        if [[ "${2:-}" != "--no-build" ]]; then
            build
        fi
        start_engine
        start_control_plane
        echo
        echo "engine:        $ENGINE_URL"
        echo "monokulo:      $CONTROL_PLANE_URL  (open this one in a browser)"
        echo
        echo "First time only: opening $CONTROL_PLANE_URL now redirects to its own"
        echo "first-run admin setup wizard - create the one admin account there."
        echo "You'll land on its admin settings page (/dashboard/admin/settings),"
        echo "which already has the engine connection pre-wired - the engine half"
        echo "of it (/dashboard/admin/invites, /dashboard/admin/settings) should"
        echo "work immediately with no further setup."
        echo
        echo "logs:          scripts/dev-run.sh logs"
        ;;
    stop)
        stop_one "monokulo" "$CP_PID_FILE"
        stop_one "engine" "$ENGINE_PID_FILE"
        ;;
    restart)
        "$0" stop
        "$0" start "${2:-}"
        ;;
    status)
        status_one "engine" "$ENGINE_PID_FILE" "$ENGINE_URL"
        status_one "monokulo" "$CP_PID_FILE" "$CONTROL_PLANE_URL"
        ;;
    logs)
        target="${2:-both}"
        case "$target" in
            engine) tail -f "$ENGINE_LOG" ;;
            monokulo) tail -f "$CP_LOG" ;;
            both) tail -f "$ENGINE_LOG" "$CP_LOG" ;;
            *)
                echo "unknown log target: $target (expected: engine, monokulo, or nothing for both)" >&2
                exit 2
                ;;
        esac
        ;;
    -h|--help|"")
        usage
        ;;
    *)
        echo "unknown command: $cmd" >&2
        usage
        exit 2
        ;;
esac
