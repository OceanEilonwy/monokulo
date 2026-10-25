# Audit decisions

Calls made during the 2026-10-25 code audit and the fixes that follow it,
newest last. The findings, plans and pull requests themselves are in the
audit database (`target/code-audit/audit.db`).

## Made by the user

- **Test policy.** Every added test has to earn its place. A fix that makes
  a bug impossible by construction gets no test, unless the property is
  critical (payments, keys, privacy/Tor, cross-store authorization). Tests
  extend existing ones wherever they can.
- **#40:** the public key (`pk_`) gets its own idempotency namespace, apart
  from the secret and app keys. This changes published behaviour; the API
  recording and docs change with it.
- **#96:** treated as critical (privacy).
- **#32:** retire `http_cache.max_mb` cleanly: a retired key in the options
  file, env or CLI warns once instead of stopping startup, and a migration
  deletes the stored row.
- **Severities:** #39 and #99 raised to critical, #17 to high; #107 lowered
  to low.
- **Onion Monero node host:** refused when a node setting is saved; an
  existing one still starts, with a boot warning and a status-page note
  (it would leak the onion name to the system DNS; monerod over Tor is
  out of scope).
- **#4 and #161, how block bodies are verified:** only bodies that matched
  a store are fetched again through `/get_transactions` and checked against
  their ids (PR 173). Verifying every body from `get_blocks.bin` was the
  first choice, but real nodes (four checked, including
  node.hollingworth.xyz) send its `prunable_hash` as all zeros, and a
  second download of every block's transactions doesn't scale. #161 (a
  lying node hiding a payment) stays open at medium, documented as trust
  in the node; issue #177 records the zeroed hash, which the user is
  raising with the Monero developers.
- **Merging:** Claude merges each fix PR once CI is green and it has
  reviewed the diff.
- **Startup data writes go through the migration system.** #175 drops its
  startup backfill of SEV-SNP key checks (a check is stored only when a
  wrap is written). The firmware-update rewrap, which already rewrote the
  wrap at start outside migrations, moves into the migration system as
  tracked code steps run after unseal, one per firmware version, in its own
  PR after #175.
- **#175's guest-SVN-0 break:** accepted; no instance uses SEV-SNP yet.
- **Fix order:** critical groups first, then high, then the cross-crate type
  groups, then medium, then low bundles; one pull request per fix group.

## Made during the audit

- Scope: Rust only, excluding `e2e/` and `crates/e2e-harness`.
- Duplicates merged: #8 into #15, #92 into #26, #50 into #106 (and, by
  verifiers, #140 into #15, #149 into #1, #158 into #43).
- Fix groups merged where one change covers them: the wallet-change and
  store-delete races share one per-store lock (`tenant-lock-races`); the
  currency and idempotency findings join `order-creation-single-path`;
  #76 lands with #143 (`event-kinds-and-followers`) so a strict event-kind
  enum can't stop every store's webhooks on version skew.
- #161 recorded from the independent review of PR 173: a lying node can
  still hide a payment by swapping a block body for one that pays nobody.
  Pre-existing; fixed separately.
- PR 175 (#39): the key check and stored-version check can't stop a host
  that already knows the planted master (they are host-editable rows); the
  release guest-SVN floor in the measured image is the guard the host
  can't edit, so a flawed image is answered by raising that floor. An image
  signed at guest SVN 0 no longer starts (the floor is 1); self-hosters who
  signed at 0 must re-sign.
- PR 173 (#4): a mismatched re-fetch is a node error that evicts the cached
  block; follow-ups from its review (failover on an empty answer, the test
  fake's default binding) go into the same PR.
- Fix agents build in `target/fix-slot-N` with 6 build jobs each, so several
  can build at once without running out of memory.
