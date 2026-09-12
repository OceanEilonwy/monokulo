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
