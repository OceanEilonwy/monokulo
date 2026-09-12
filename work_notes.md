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

_(empty — nothing implemented yet; entries appended here as each WBS item
lands, newest last)_

## Judgment calls & open questions for the user

_(empty so far — record anything decided without waiting for input here,
with enough context that the user can override it later if it was wrong)_
