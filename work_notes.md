# Work Notes — MoneroPay Cloud / WooCommerce MVP

Running hand-off brief for agents implementing `docs/WOOCOMMERCE_WBS.md`.
Read this file first, then the specific WBS item(s) you've been assigned —
this file gives you the state and context; the WBS gives you the spec.

## What this project is

`moneropay-core` (repo root) is an existing, working, self-hosted Monero
payment gateway (Rust/axum/SQLite). We're extending it into a hosted SaaS
("MoneroPay Cloud") with a WooCommerce integration as the first platform.
Full rationale: `docs/WOOCOMMERCE_ROADMAP.md`. Full task breakdown:
`docs/WOOCOMMERCE_WBS.md`. Read both before assuming anything not stated
here — this file is a summary, not the source of truth.

Key architectural facts an agent should not have to rediscover:
- The existing engine crate (root `Cargo.toml`, `src/`) is becoming one
  member of a Cargo workspace, alongside new `shared/`, `control-plane/`,
  and `mock-woocommerce/` crates. No existing engine file moves.
- `shared/` holds logic pulled out of the engine (secret-token hashing,
  HMAC webhook signing, the migration runner) plus genuinely new helpers
  the engine doesn't have (argon2 password hashing).
- The engine's `src/lib.rs` already exports everything (`http`, `store`,
  `key_custody`, etc.) as `pub mod` — it's a real, usable library
  dependency for other workspace crates, not just a binary.
- `tests/e2e_stagenet.rs` already demonstrates the pattern for driving the
  engine directly (build the router, use it against real or in-memory
  storage) — reuse that pattern, don't reinvent it.

## Current repo state

- Working in git worktree `/home/henry/Downloads/mokulo/.claude/worktrees/woocommerce-roadmap-doc`,
  branch `worktree-woocommerce-roadmap-doc`. **This branch is not pushed to
  origin** (push access denied under current credentials) — it only exists
  locally. Do not assume it's recoverable from GitHub.
- All work so far is documentation: `docs/WOOCOMMERCE_ROADMAP.md` and
  `docs/WOOCOMMERCE_WBS.md`, both current and cross-checked against the
  real code as of this session (see the WBS's "gaps found" commit for what
  was corrected).
- No implementation code has been written yet. WBS item 0.1 (workspace
  setup) has not started as of this note.

## Progress log

- **Foundations (0.1-0.6) complete.** Track A starts next (control-plane
  accounts, WBS 1.1).
- 0.6 done: real-engine test harness landed as its **own new crate**,
  `engine-test-support/`, not inside `shared` — correctly identified a real
  circular-dependency problem (`moneropay-core` depends on `shared`, so a
  harness needing `moneropay-core` can't live in `shared` without a cycle)
  and resolved it exactly as the WBS's own hedge anticipated ("in `shared`,
  or a dev-only sibling crate"). Layering:
  `engine-test-support -> moneropay-core -> shared`. Public API:
  `spawn_test_engine() -> TestEngineHandle` (in-memory `Store`,
  `PlainKeyCustody`, empty `FixedRateProvider`, no configured networks,
  bound to a real `127.0.0.1:<ephemeral-port>` via `axum::serve` in a
  background task; `Drop` aborts the task). No `#[cfg(test)]`/feature gate
  needed on the crate itself — being reached only via `[dev-dependencies]`
  is what keeps it out of real builds. `mock-woocommerce` now depends on it
  as a dev-dependency. Smoke test does a genuine `reqwest` round trip
  (confirmed: real TCP, not `tower::ServiceExt::oneshot`) against
  `/static/moneropay-client.js` (verified dependency-free — no tenant/node/
  scanner needed). Engine/shared test counts unaffected (269/24); new
  crate at 1 passed. Independently re-verified (full file read, workspace
  member diff, test re-run) before commit.
- 0.5 done: `shared::migrations::apply` now holds the generic transactional
  migration runner (moved from `src/store.rs`'s `apply_migration_list`,
  renamed since it's namespaced now — pure move, `unchecked_transaction`
  usage and all-or-nothing semantics unchanged). Engine keeps its own
  `MIGRATIONS` list (the `include_str!(...)` paths, meaningless outside
  the engine crate), the `apply_migrations` wrapper (now a one-line call
  into `shared::migrations::apply`), and `configure_connection`
  (`PRAGMA foreign_keys` etc.) — confirmed the required ordering
  (`configure_connection` then `apply_migrations`, outside any
  transaction) is untouched at both call sites. Good judgment call on
  which tests moved: the generic-mechanism test
  (`a_failing_migration_leaves_neither_its_schema_changes_nor_its_version_row`,
  built on an ad-hoc migration list) moved to `shared`; two tests that
  drive the *real* `MIGRATIONS`/`Store` schema
  (`reopening_an_existing_database_file_does_not_reapply_migrations`,
  `migration_0004_rebuilds_order_payments_without_losing_existing_rows`)
  correctly stayed in `store.rs` since they test engine schema content,
  not the runner itself — only their call site was updated to
  `shared::migrations::apply(...)`. Engine 269 passed/8 ignored (was 270,
  −1 moved), `shared` 24 passed (was 23, +1). Independently re-verified
  (diff read in full, ordering confirmed, tests re-run) before commit.
- 0.4 done: `shared::password` — new, genuinely new logic (not a move),
  Argon2id via the `argon2` crate for the control plane's future human
  account passwords, explicitly separate from `shared::auth`'s SHA-256
  token hashing (different threat model, documented in the module's own
  doc comment). Pinned `argon2 = "0.5"` (resolved to 0.5.3) rather than
  the `cargo add`-default 0.6.0 — 0.6 ships a rewritten `password-hash`
  0.6.1 API without `SaltString`/`rand_core`, not the standard
  SaltString+OsRng+PHC-string pattern; 0.5's API is the well-documented,
  idiomatic one and was what was actually wanted here. `hash_password`
  returns a self-describing PHC-format string; `verify_password` returns
  `false` uniformly for both "wrong password" and "malformed hash string"
  (no panic, no distinguishable side channel). 4 new tests (round-trip,
  wrong password, per-call-random-salt via two different hashes of the
  same password, malformed-input handling). `shared` now 23 passed
  (was 19); engine unaffected at 270. Independently re-verified before
  commit.
- 0.3 done: `shared::webhook_sign` now holds HMAC signing/verification
  *and* the SSRF URL-validation logic (`validate_webhook_url`,
  `is_disallowed_address`, `WebhookUrlError`) moved from `src/webhook_sign.rs`
  — same file covered both concerns originally. Same thin-re-export
  pattern as 0.2; only real call site is `src/webhook_delivery.rs`. Added
  a new known-vector test (on top of one that already existed and moved
  over) specifically for the later PHP webhook-receiver task (WBS 1.5.4) to
  cross-check against:
  - secret: `known_vector_secret_for_php_crosscheck`
  - payload: `{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}`
  - expected signature (hex, lowercase): computed by the test itself from
    the real `sign_payload` function — see
    `shared::webhook_sign::tests::known_vector_for_cross_language_php_verification`
    for the exact value rather than retyping it here (avoids a transcription
    error propagating into the eventual PHP test).
  - PHP-side equivalent: `hash_hmac('sha256', PAYLOAD, SECRET)`, compared
    with `hash_equals()`, not `==` (per the constant-time requirement noted
    in the WBS at 1.5.4).
  Engine 270 passed/8 ignored (was 285, −15 moved), `shared` 19 passed
  (3 + 15 moved + 1 new). Independently re-verified before commit.
- 0.2 done: `shared::auth` now holds the token generation/hashing logic
  moved from `src/auth.rs` (SHA-256, unchanged). `src/auth.rs` is a thin
  `pub use shared::auth::*;` re-export so `src/http/admin.rs` and
  `src/store.rs` (the only call sites) needed no changes. Root `Cargo.toml`
  depends on `shared` by path now. Tests moved intact: engine 285
  passed/8 ignored (was 287 — the 2 moved tests now run from `shared`,
  which is at 3 passed total including its own placeholder). Independently
  re-verified (diff + full `cargo test --workspace` re-run) before commit.
- 0.1 done: root `Cargo.toml` gained a `[workspace]` table
  (`members = ["shared", "control-plane", "mock-woocommerce"]` — the root
  package is included implicitly since it already has `[package]`; no
  separate "." entry needed or accepted). Added `shared/` (lib crate,
  empty placeholder + trivial test), `control-plane/` (bin crate, `fn
  main() {}` + trivial test), and `mock-woocommerce/` (bin crate, `fn
  main() {}` + trivial test, with `moneropay-core = { path = ".." }` as a
  real dependency for later integration tests). `cargo build --workspace`
  and `cargo test --workspace` both succeed: engine 287 passed/8 ignored
  (unchanged from pre-workspace baseline), shared/control-plane/
  mock-woocommerce each 1 passed. No existing engine file touched other
  than the new `[workspace]` table in `Cargo.toml`.

## Judgment calls & open questions for the user

- **Important: this worktree is built on an older baseline than your main
  checkout, and it matters for one specific area.** While reviewing 0.1's
  work I found that the docs (`docs/WOOCOMMERCE_WBS.md`,
  `docs/WOOCOMMERCE_ROADMAP.md`) and my briefing to the 0.1 agent both
  contained a claim — "the engine's `Cargo.toml` already has an `[[test]]`
  section and optional `e2e` feature gating `monero-wallet`/
  `monero-daemon-rpc`/etc." — that came from reading
  `/home/henry/Downloads/mokulo/Cargo.toml` (your main checkout) during the
  earlier gap-analysis pass, *not* this worktree. Your main checkout has
  uncommitted local changes (`git status` there shows `Cargo.toml`,
  `e2e/.gitignore`, `e2e/README.md`, `e2e/stagenet-wallets.json`,
  `src/cli.rs`, `src/config.rs`, `src/http/mod.rs`, `src/lib.rs`,
  `src/main.rs`, `tests/e2e_stagenet.rs`, `tests/support/mod.rs` modified,
  plus new untracked `src/e2e_wallet.rs`, `src/http/e2e_dev.rs`,
  `static/e2e-shop.html`) that this worktree — branched from the last
  *commit*, per how `EnterWorktree` works — never received, since
  uncommitted changes in one working tree aren't visible from another.
  - **What I checked to scope the actual impact**: re-read this worktree's
    real `src/lib.rs`, `src/http/mod.rs` (router table, `AuthedTenant`'s
    `Bearer` parsing), and `Cargo.toml` directly. The router paths, the
    `Authorization: Bearer sk_...` auth format, the migration runner, the
    `KeyCustody` trait, the rate limiter, HMAC signing, and the absence of
    `argon2`/`governor` — everything this WBS's implementation tasks
    actually depend on — are identical in both places. The only thing
    that's genuinely different is your in-progress e2e-tooling refactor
    (feature-gating the real-transaction-construction dependencies, a
    `--e2e` dev-server mode, a demo shop page) — unrelated to the
    WooCommerce/control-plane/SEV-SNP work, as far as I can tell.
  - **What I did about it**: nothing destructive — I left your main
    checkout completely untouched and am continuing to build in this
    worktree, since copying or guessing at unfinished WIP from outside it
    seemed riskier than proceeding on the last committed state. I fixed the
    incorrect claim in the 0.1 agent's task (it correctly reported the
    `[[test]]`/`e2e`-feature structure wasn't actually present, which I'd
    initially mis-read as *it* being wrong — it wasn't, I was, for briefing
    it off the wrong checkout).
  - **What you'll want to do when you're back**: decide whether to commit
    that e2e-tooling WIP, and if so, merge/rebase this branch
    (`worktree-woocommerce-roadmap-doc`, currently local-only — recall push
    to `origin` is denied under current credentials) on top of it once it
    lands. Until then I'll keep treating this worktree's committed baseline
    as ground truth and will flag it again if a later task's diff would
    touch any of the files listed above, since those are the ones a future
    merge will need to reconcile.
