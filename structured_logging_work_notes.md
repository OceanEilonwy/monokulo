# Structured logging: work notes

Progress notes for `structured_logging.md`, kept so another agent can pick up
where the last one stopped. Newest state first in "Where things stand";
decisions and surprises below it.

Branch: `structured-logging` (branched from `admin-settings-v2` at c7c95dc,
because it builds on the `live-settings` crate from that work).

## How to resume

1. `git checkout structured-logging && git log --oneline admin-settings-v2..`
   shows what is done.
2. Read "Where things stand" below, then the plan section it points at.
3. Checks before every commit:
   `cargo test -p telemetry -p shared -p live-settings`,
   `cargo test -p scanner -p monokulo` (slow),
   `cargo clippy --workspace --all-targets`.

## Where things stand

| Task | State | Commit | Notes |
|---|---|---|---|
| Plan | done | 741b108 | `structured_logging.md` |
| 1.1 `crates/telemetry` | done | (this commit) | init, JSON and pretty output, reload handle |
| 1.2 move log calls to `tracing` | done for server code | (this commit) | see "What was left on println/eprintln" |
| 1.3 level and dev mode as live settings | settings + apply done; admin page control not yet | (this commit) | see "Next" |
| 1.4 redaction | done | (this commit) | `crates/telemetry/src/redact.rs` |
| 2.x onwards | not started | | |

### Next

- 1.3, admin page: `logging.dev_mode_until` currently shows as a plain
  number input (Unix seconds) on the admin settings page, for both
  monokulo's and the engine's settings. Give it its own control in
  `crates/monokulo/src/views/admin.rs` (`scalar_input`, keyed on the setting
  key): a select of "Off", "On for 1 hour", "4 hours", "24 hours", whose
  option values are absolute Unix times computed when the page is rendered,
  plus a line saying "On until <time> UTC" when it is on. No server-side
  parsing change is needed, since the posted value is still a Unix time.
  Add "Logging" to `engine_group` in the same file so the engine's two
  logging settings get their own heading instead of "Other".
- Then part 2 (spans and trace propagation).

## Decisions and deviations from the plan

- **Development mode is a time, not a switch plus a duration.** The plan
  said `logging.dev_mode` (bool) plus `logging.dev_mode_minutes`. Built
  instead: one setting, `logging.dev_mode_until` (Unix seconds, 0 = off).
  Reasons: a restart during the window keeps the same end time (a bool would
  start a new hour, or need extra stored state); the admin page shows the
  true state without the process having to write its own setting back when
  the time runs out; and the expiry is just a timer in `Telemetry::apply`.
- **Development mode changes the level only, not the output format.** The
  plan said pretty output and span events in development mode. Switching a
  production process's stderr from JSON to text mid-run would break whatever
  parses its logs (journald JSON, a collector). Instead the format is chosen
  once at start: `<PREFIX>_LOG_FORMAT=json|pretty`, else pretty at a
  terminal and JSON otherwise. Span open/close events are not emitted; part
  2's HTTP spans will carry durations as fields instead.
- **Env variable names.** Level: `SCANNER_LOG`, `MONOKULO_LOG`,
  `KEY_CUSTODY_LOG` (the key-custody server has no settings, only the env
  variable). These are also the `logging.level` settings' env variables, so
  the variable is in effect from the first line and keeps winning after the
  settings load. `RUST_LOG` is not read.
- **Redaction is done where values are formatted**, not by a layer that
  removes fields (tracing layers can't change an event another layer sees).
  Both output formats call `telemetry::redact`. The log store (part 3) must
  reuse `json::JsonVisitor` or call `redact::field` the same way.
- **Redaction rules** (all in `redact.rs`, all unit tested): secret field
  names are replaced by `[redacted]`; client address fields keep only their
  network (IPv4 /24, IPv6 /48; loopback and private kept); Monero addresses
  in any string are shortened to 6 + 4 characters; `sk_<hex>` store keys in
  any string lose everything after `sk_`. Hex strings (txids, key images)
  are not touched: public data and needed for debugging. There is no keyed
  hashing, so nothing needs a per-install key.
- **Monokulo "connections" are logged as `store.id`** (a connection is a
  store in the UI), so one attribute name works across monokulo and the
  engine for the planned "everything about store X" view.
- **`shared::log::throttled` became the `shared::throttled!` macro**, which
  takes a key, a level and ordinary `tracing` fields, and adds
  `throttle_key` and `suppressed` fields. `shared` re-exports `tracing` for
  it.
- **`LogReloadable` applies to the process-wide subscriber** found through
  `telemetry::global()`, so test engines and test monokulo instances (which
  never call `telemetry::init`) register it and it does nothing. That kept
  every existing `EngineSettings::load*` / `MonokuloSettings::load` call
  site unchanged.

## What was left on println/eprintln, on purpose

- CLI output meant for the person at the terminal: the engine's one-off
  commands (`--rotate-secret`, `--show-tenant`, `--bootstrap-wallet`, the
  usage error) and the "generated a new instance admin token" line, which
  shows a secret once and must never go to a log.
- Test tools and harnesses: `crates/scanner/src/bin/e2e_harness.rs`,
  `crates/scanner-test-support` (including `fake-monerod`, whose
  `FAKE_MONEROD_READY` stdout line the Playwright setup waits for),
  `crates/cli-wallet`, `crates/snp-attest`, `crates/mock-woocommerce`,
  `xtask`, and `println!` inside tests.

## Things to know

- Nothing in the repo parses the servers' stdout: the Playwright
  real-binaries setup waits by polling HTTP and the socket, so moving the
  "listening on" lines to `tracing` (stderr) broke nothing.
- Tests don't install a subscriber, so log output from code under test is
  no longer printed in failing test output (it used to be, via
  `eprintln!`). If that is missed, a test helper can call
  `telemetry::build(..)` and `tracing::subscriber::set_default`.
