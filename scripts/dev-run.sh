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
# engine's own real SQLite database, the monokulo's own real SQLite
# database, and a locally-generated MONOKULO_ENCRYPTION_KEY. Nothing
# here is checked in, and nothing here is a real credential worth
# protecting beyond your own machine - this is a local dev stack against
# a real *stagenet* wallet (worthless XMR only), the same wallet
# e2e/moneropay-stagenet.toml already uses for the repo's own real
# end-to-end test.
#
# ENGINE_URL/CONTROL_PLANE_URL below are fixed, not configurable via a
# flag: the monokulo's own src/main.rs currently hardcodes
# "http://127.0.0.1:8080" as the engine it talks to (a real, documented
# placeholder - see that file's own TODO), so the engine's bind address
# genuinely cannot be anything else for this pairing to work.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_DIR="$REPO_ROOT/.dev-run"
ENGINE_DIR="$RUN_DIR/engine"
CP_DIR="$RUN_DIR/monokulo"

ENGINE_BIN="$REPO_ROOT/target/debug/scanner"
CP_BIN="$REPO_ROOT/target/debug/monokulo"

ENGINE_CONFIG="$ENGINE_DIR/moneropay.toml"
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

ensure_dirs() {
    mkdir -p "$ENGINE_DIR" "$CP_DIR"
}

# Reuses the repo's own real e2e stagenet config (same worthless test
# wallet the real end-to-end test already uses) rather than inventing a
# second one to keep in sync - only the bind address is overridden, to
# the fixed port monokulo's own hardcoded EngineClient URL expects
# (see this script's own header comment).
ensure_engine_config() {
    if [[ -f "$ENGINE_CONFIG" ]]; then
        return
    fi
    echo "==> writing a dev engine config (from e2e/moneropay-stagenet.toml) to $ENGINE_CONFIG"
    sed 's#^bind = .*#bind = "127.0.0.1:8080"#' "$REPO_ROOT/e2e/moneropay-stagenet.toml" > "$ENGINE_CONFIG"
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

start_engine() {
    if is_running "$ENGINE_PID_FILE"; then
        echo "engine already running (pid $(cat "$ENGINE_PID_FILE"))"
        return
    fi
    ensure_engine_config
    if [[ ! -x "$ENGINE_BIN" ]]; then
        echo "error: $ENGINE_BIN not found - run without --no-build at least once" >&2
        exit 1
    fi
    echo "==> starting engine -> $ENGINE_LOG"
    nohup "$ENGINE_BIN" --config "$ENGINE_CONFIG" > "$ENGINE_LOG" 2>&1 &
    echo $! > "$ENGINE_PID_FILE"
    sleep 1
    if is_running "$ENGINE_PID_FILE"; then
        echo "    engine up (pid $(cat "$ENGINE_PID_FILE")) - $ENGINE_URL"
    else
        echo "    engine failed to start - see $ENGINE_LOG" >&2
        rm -f "$ENGINE_PID_FILE"
        exit 1
    fi
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
    (
        cd "$CP_DIR"
        MONOKULO_ENCRYPTION_KEY="$(cat "$CP_KEY_FILE")" nohup "$CP_BIN" > "$CP_LOG" 2>&1 &
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
