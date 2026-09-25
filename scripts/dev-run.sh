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
# safe to run again while already running - it just reports what's up.
#
# WHERE STATE LIVES: everything this script creates lives under
# .dev-run/ at the repo root (gitignored) - PID files, logs, the
# engine's own real SQLite database (settings, tenant, everything - there
# is no config file any more, see below), the monokulo's own real SQLite
# database, and two locally-generated secrets (MONOKULO_ENCRYPTION_KEY and
# the engine's own instance-admin token). Nothing here is checked in, and
# nothing here is a real credential worth protecting beyond your own
# machine - this is a local dev stack against a real *stagenet* wallet
# (worthless XMR only), the same wallet e2e/moneropay-stagenet.toml
# already uses for the repo's own real end-to-end test.
#
# NO CONFIG FILE ANY MORE. The engine used to be started with
# `--config moneropay.toml`; that flag (and the whole TOML config format)
# is gone - every setting now lives in the engine's own `settings` table,
# readable/editable at runtime over its instance-admin HTTP API
# (`GET`/`POST /api/v1/admin/settings`) rather than only at boot from a
# file. This script provisions the same dev-friendly values the old TOML
# had (the same stagenet node, the same fast-iteration payment thresholds)
# by POSTing them to that API right after the engine's first boot - see
# ensure_engine_settings() below - and mints/reuses a fixed instance-admin
# token to authenticate those calls (ENGINE_ADMIN_TOKEN_FILE), the same
# "generate once, persist, reuse" treatment this script already gives
# MONOKULO_ENCRYPTION_KEY. That same token is also handed to monokulo as
# MONOKULO_SCANNER_ADMIN_TOKEN, so monokulo's own admin settings/invites
# pages can reach the engine's admin API immediately - see
# `crates/monokulo/src/http/admin_settings.rs`'s own module doc comment
# for what that page actually does with it.
#
# ENGINE_URL is fixed at 127.0.0.1:8080 (via SCANNER_SERVER_BIND below),
# matching monokulo's own MONOKULO_ENGINE_URL setting default - genuinely
# just a *default* now on both sides (both are real, database-backed
# settings, not hardcoded), but this script still pins the engine's side
# explicitly so the pairing is correct out of the box without the operator
# having to configure anything through the browser first.
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

ENGINE_BIN="$REPO_ROOT/target/debug/scanner"
CP_BIN="$REPO_ROOT/target/debug/monokulo"

ENGINE_DB="$ENGINE_DIR/scanner.db"
ENGINE_ADMIN_TOKEN_FILE="$ENGINE_DIR/admin_token.txt"
CP_KEY_FILE="$CP_DIR/encryption_key.txt"

ENGINE_PID_FILE="$RUN_DIR/engine.pid"
CP_PID_FILE="$RUN_DIR/monokulo.pid"
ENGINE_LOG="$RUN_DIR/engine.log"
CP_LOG="$RUN_DIR/monokulo.log"

ENGINE_URL="http://127.0.0.1:8080"
CONTROL_PLANE_URL="http://127.0.0.1:8081"

# The same real stagenet node + dev-friendly payment thresholds
# e2e/moneropay-stagenet.toml used to provide via its own TOML sections -
# see that file (still present, used by the real end-to-end tests) and
# crates/scanner/tests/support/mod.rs's own e2e_fixture module, which
# extracted these exact same values into Rust constants for the same
# reason (the TOML file is no longer parsed by the engine itself, only
# read by humans/tests as a reference).
STAGENET_NODE_HOST="node.monerodevs.org"
STAGENET_NODE_PORT=38089
STAGENET_NODE_FALLBACK_HOST="node2.monerodevs.org"
STAGENET_NODE_FALLBACK_PORT=38089
PAYMENT_CONFIRMATIONS_REQUIRED=0
PAYMENT_ORDER_EXPIRY_MINUTES=30
PAYMENT_REORG_CHECK_DEPTH=20
PAYMENT_MEMPOOL_POLL_INTERVAL_MS=2000

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

ensure_dirs() {
    mkdir -p "$ENGINE_DIR" "$CP_DIR"
}

ensure_engine_admin_token() {
    if [[ -f "$ENGINE_ADMIN_TOKEN_FILE" ]]; then
        return
    fi
    if ! command -v openssl >/dev/null 2>&1; then
        echo "error: openssl not found - needed once, to generate a dev engine instance-admin token" >&2
        exit 1
    fi
    echo "==> generating a dev engine instance-admin token (persisted at $ENGINE_ADMIN_TOKEN_FILE, reused on every future start)"
    openssl rand -hex 32 > "$ENGINE_ADMIN_TOKEN_FILE"
    chmod 600 "$ENGINE_ADMIN_TOKEN_FILE"
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
    echo "==> building scanner and monokulo (debug)"
    (cd "$REPO_ROOT" && cargo build -p scanner --bin scanner -p monokulo --bin monokulo)
}

# Provisions the same dev-friendly stagenet node + payment thresholds the
# old TOML config gave the engine, over its own instance-admin HTTP API -
# see this script's own header comment on why this replaces a config
# file. Safe (and cheap) to re-run on every `start`: every value here is
# fixed, so re-POSTing it just re-saves the same thing. Retries briefly
# since this runs right after the server process is spawned - `is_running`
# only proves the process exists, not that it's finished binding yet.
ensure_engine_settings() {
    local token url attempt
    token="$(cat "$ENGINE_ADMIN_TOKEN_FILE")"
    url="$ENGINE_URL/api/v1/admin/settings"
    local body
    body=$(cat <<EOF
{
  "scalars": {
    "payment.confirmations_required": "$PAYMENT_CONFIRMATIONS_REQUIRED",
    "payment.order_expiry_minutes": "$PAYMENT_ORDER_EXPIRY_MINUTES",
    "payment.reorg_check_depth": "$PAYMENT_REORG_CHECK_DEPTH",
    "payment.mempool_poll_interval_ms": "$PAYMENT_MEMPOOL_POLL_INTERVAL_MS"
  },
  "monero_node": {
    "stagenet": {
      "host": "$STAGENET_NODE_HOST",
      "port": $STAGENET_NODE_PORT,
      "ssl": false,
      "accept_self_signed_certs": true,
      "fallbacks": [
        {
          "host": "$STAGENET_NODE_FALLBACK_HOST",
          "port": $STAGENET_NODE_FALLBACK_PORT,
          "ssl": false,
          "accept_self_signed_certs": true,
          "fallbacks": []
        }
      ]
    }
  }
}
EOF
)
    for attempt in $(seq 1 10); do
        if curl -sf -o /dev/null -X POST "$url" -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d "$body"; then
            echo "    engine settings provisioned (stagenet node + dev payment thresholds)"
            return
        fi
        sleep 0.3
    done
    echo "    warning: could not reach $url to provision engine settings - is the engine actually up? (see $ENGINE_LOG)" >&2
}

# Provisions the one self-hosted tenant, from the same watch-only stagenet
# wallet the repo's own e2e tests use - a one-time action (refuses once a
# tenant already exists), so this only ever does real work on the very
# first `start` against a fresh database. Deliberately called *after*
# ensure_engine_settings, not before: `--bootstrap-wallet` bakes the
# tenant's own confirmations_required/order_expiry_minutes
# in from whatever this instance's *current* settings are at the moment it
# runs (`local_admin::bootstrap_wallet`'s own doc comment) - running it
# first would bootstrap against the engine's hardcoded defaults instead of
# this script's dev-friendly overrides.
ensure_wallet_bootstrapped() {
    local out
    if out=$(SCANNER_DB_PATH="$ENGINE_DB" "$ENGINE_BIN" --bootstrap-wallet \
        --primary-address "54F1KdjaAtnL6Fb4SbLUM1AMQSjSERjYUgYRtVgwjBirA26RyJCzxc4TbWPW65ZvRC6bifBfrTTv3fyu25BFQuvA2ogNiXg" \
        --view-key "fcdc7998f003928b3f409b94d54f690d16ca6df3689de4da4803c5a9c792fb0e" \
        --spend-pubkey "3fa2161d4e2cc7722288d33e46a4cc37e92629d7e45939ec67cc42e8f144b335" \
        --network stagenet \
        --allowed-origins "http://127.0.0.1:8190" 2>&1); then
        echo "==> bootstrapped the dev stagenet tenant:"
        echo "$out" | sed 's/^/    /'
    fi
    # A non-zero exit here just means "already bootstrapped" (a tenant
    # from a previous run's database) - not an error worth failing
    # `start` over, so deliberately not checked against `set -e`.
}

start_engine() {
    if is_running "$ENGINE_PID_FILE"; then
        echo "engine already running (pid $(cat "$ENGINE_PID_FILE"))"
        return
    fi
    ensure_engine_admin_token
    if [[ ! -x "$ENGINE_BIN" ]]; then
        echo "error: $ENGINE_BIN not found - run without --no-build at least once" >&2
        exit 1
    fi
    echo "==> starting engine -> $ENGINE_LOG"
    SCANNER_DB_PATH="$ENGINE_DB" \
    SCANNER_SERVER_BIND="127.0.0.1:8080" \
    SCANNER_ADMIN_TOKEN="$(cat "$ENGINE_ADMIN_TOKEN_FILE")" \
        nohup "$ENGINE_BIN" > "$ENGINE_LOG" 2>&1 &
    echo $! > "$ENGINE_PID_FILE"
    sleep 1
    if is_running "$ENGINE_PID_FILE"; then
        echo "    engine up (pid $(cat "$ENGINE_PID_FILE")) - $ENGINE_URL"
    else
        echo "    engine failed to start - see $ENGINE_LOG" >&2
        rm -f "$ENGINE_PID_FILE"
        exit 1
    fi
    ensure_engine_settings
    ensure_wallet_bootstrapped
}

start_control_plane() {
    if is_running "$CP_PID_FILE"; then
        echo "monokulo already running (pid $(cat "$CP_PID_FILE"))"
        return
    fi
    ensure_cp_key
    if [[ ! -x "$CP_BIN" ]]; then
        echo "error: $CP_BIN not found - run without --no-build at least once" >&2
        exit 1
    fi
    echo "==> starting monokulo -> $CP_LOG"
    # Run with $CP_DIR as its working directory - monokulo's own
    # main.rs opens its SQLite database at the relative path
    # "monokulo.db", so this is what makes it land (and persist
    # across restarts) under .dev-run/monokulo/ rather than
    # wherever this script happened to be invoked from.
    #
    # MONOKULO_SCANNER_ADMIN_TOKEN/MONOKULO_ENGINE_URL wire monokulo's own
    # admin settings/invites pages up to the engine's admin API out of the
    # box - see this script's own header comment.
    (
        cd "$CP_DIR"
        MONOKULO_ENCRYPTION_KEY="$(cat "$CP_KEY_FILE")" \
        MONOKULO_ENGINE_URL="$ENGINE_URL" \
        MONOKULO_SCANNER_ADMIN_TOKEN="$(cat "$ENGINE_ADMIN_TOKEN_FILE")" \
            nohup "$CP_BIN" > "$CP_LOG" 2>&1 &
        echo $! > "$CP_PID_FILE"
    )
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
        echo "which already has the engine connection pre-wired - the scanner half"
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
