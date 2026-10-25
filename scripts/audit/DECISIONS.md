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
- Fix agents build in `target/fix-slot-N` with 6 build jobs each, so several
  can build at once without running out of memory.
