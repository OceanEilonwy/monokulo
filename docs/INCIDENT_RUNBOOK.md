# Incident Runbook — Box Compromise (Monokulo)

WBS 2.3.2. Covers the scenario a hosted, multi-tenant `scanner` instance
is actually exposed to when its box is compromised: **a privacy incident, not
a funds-loss one.** This is not a hedge — it is a direct consequence of
`docs/DESIGN.md` §6.1: every wallet this system ever holds is watch-only
(private view key + public spend key only). No code path anywhere in this
repository holds, derives, or ever sees a spend key. There is no step in this
document that says "move funds" or "rotate a wallet to prevent theft," because
theft of funds from this system's own compromise is not a thing that can
happen — nothing here can spend anything.

What a box compromise *does* expose: every connected tenant's **view key**
(so the attacker can retroactively and prospectively see which incoming
transactions belong to that tenant — the exact privacy property Monero's
stealth addresses exist to deny an outside observer), plus whatever the
attacker can do with the `key_custody_backend` in use at the time (below), the
tenants' `secret_token`s and webhook signing secrets, and cleartext order
metadata (fiat amounts, merchant order IDs, buyer-adjacent data if a merchant
ever put any into a `description` field).

## 1. What is actually at risk, by component

Read this section first — it decides how loudly to react to each part of the
finding, since not every read the attacker got is equally bad.

| Asset | Exposure on box compromise | Why |
|---|---|---|
| Spend keys | **None ever.** No such key exists on this box, in this database, in this process, or in any backend `KeyCustody` implements. | §6.1 — watch-only by construction, not by policy. |
| View keys (`plain` backend) | **Full.** `PlainKeyCustody` holds unwrapped key material in-process, in cleartext memory, for as long as the process runs (`src/key_custody/plain.rs`). | This is the *documented, accepted* default for self-hosted single-tenant, where host compromise already means "the attacker owns the one wallet" regardless of `KeyCustody`. On a hosted multi-tenant box it is the worst case: every connected tenant's key at once. |
| View keys (`socket` backend) | **Full, same as above, once the process is compromised** — a live root/process compromise can simply read `key-custody-server`'s memory too, or intercept the length-prefixed protocol on the loopback/unix socket in flight. `SocketKeyCustody` (WBS 2.1.2/2.1.3) buys process *separation*, not confidentiality against a host-level attacker — it is not a hardware enclave. That is WBS 2.2 (SEV-SNP), which is **not built yet**. Do not describe an incident as "contained because we use the socket backend" — it is not, today. | `docs/DESIGN.md` §6.1's own framing: a hardware-backed implementation is what closes the "host-level compromise exposes every tenant's view key" case; the socket split alone does not. |
| `tenants.sealed_key_material` (at rest, in the SQLite file) | Depends entirely on what "sealed" means for the backend that wrote it. For `plain`, this is `PlainKeyCustody`'s own encoding, not defense against an attacker who already has filesystem + process access to the box that holds the unseal key alongside it. Treat any `sealed_key_material` recovered from a compromised box as **recoverable by the attacker**, not as safely opaque. | Never assume "it's sealed" means "it's safe" once the box that can unseal it is the box that was compromised. |
| `tenants.secret_token`, webhook signing secrets | **Full**, plaintext in the same database. | `src/store.rs` schema — these are not hashed at rest (unlike merchant-facing login passwords elsewhere in this codebase — see `src/password.rs`, which is not used for these fields). |
| Order metadata (fiat amounts, `merchant_order_id`, `description`) | **Full**, plaintext, no PII by design (this system deliberately never collects buyer identity — §2/§3 non-goals) but a merchant's own `description` field is merchant-controlled free text and could contain more than intended. | Worth calling out to affected merchants explicitly, not assumed benign. |
| Funds already sent to a watch-only address | **None at risk.** The attacker can *see* these outputs (once they have the view key) but cannot spend them, redirect them, or reverse them. | Same watch-only guarantee as row 1. |

## 2. Immediate response (first 30 minutes)

1. **Isolate, don't destroy.** Pull the box off the network (security group /
   firewall deny-all, or physically disconnect) rather than powering it off —
   a live memory image is the only way to later confirm *whether* key
   material was actually read versus merely reachable, and powering off loses
   that. If the box must be preserved for forensics, a hypervisor-level
   snapshot beats a `poweroff`.
2. **Rotate what can be rotated, and only what can be.** View keys
   fundamentally cannot be "rotated" the way a password can — a Monero
   subaddress's view key is intrinsic to the addresses already generated
   under it, so "rotating" really means **onboarding the tenant onto a new
   wallet** (new spend key, generated by the tenant themselves — see §3,
   non-goals: this system never generates spend keys) and migrating them to
   it going forward. That is a merchant-facing, non-code decision (§4 below),
   not an on-call action. What *can* be rotated immediately, unilaterally, by
   an operator:
   - Every `secret_token` (`scanner --rotate-secret`, per tenant —
     `docs/DESIGN.md` §4.1's onboarding-tooling flags operate directly on the
     local SQLite file, exactly the operation this calls for).
   - Every webhook signing secret (same admin surface).
   - Any monokulo-issued OAuth/connect-flow tokens still outstanding
     (`monokulo`'s `connect_tokens` — these are already single-use with
     a 10-minute TTL per `docs/WOOCOMMERCE_WBS.md`'s connect-flow spec, so
     the exposure window for an *unused* token is already small, but any
     issued-and-unconsumed token from before the compromise should be treated
     as burned).
   - Any operator/admin credentials used to reach this box at all (SSH keys,
     cloud console access, the admin API's own auth if a separate admin
     credential exists for it per §10.1).
3. **Do not restart the compromised process against the same box.** Bring the
   service back up on a *clean* box, restored from the most recent verified
   backup (`scripts/restore-database.sh` — see WBS 2.3.1 and its own written
   procedure in that script's header) — never by patching and reusing the
   compromised one. A box that was compromised once is not trusted to be
   clean just because the immediate foothold was closed.
4. **Preserve evidence before any cleanup**: process list, open file
   handles/sockets (was `key-custody-server`'s unix socket reachable from
   somewhere it shouldn't have been?), auth logs, and a copy of the database
   file *as found* (separate from the clean restore in step 3) — needed for
   §3's scope determination and for any legal/compliance notification
   obligations in the affected merchants' jurisdictions.

## 3. Scope determination

Answer these, in order, before drafting any merchant notification — an
imprecise scope either needlessly alarms unaffected merchants or, worse,
misses affected ones:

1. **Which tenants were live on this box at the time of compromise?**
   `SELECT id, public_key, key_custody_backend, disabled_at FROM tenants` —
   every non-`disabled_at` row is presumptively affected; a `disabled_at`
   tenant's key material is still present in the database and must be
   presumed affected too unless it was demonstrably purged.
2. **What was the actual attacker foothold and its privilege level?** A
   read-only information-disclosure bug reaching, say, only the HTTP read
   pool (`docs/DESIGN.md` §5's read-pool component, which never touches
   `KeyCustody`) is a materially smaller incident than root/process-level
   access that can read `key-custody-server`'s memory or the database file
   directly. Do not default to "assume the worst" for the *notification*
   without checking — but *do* default to the worst for the *response* in §2
   above, since containment must not wait on a slow forensic answer.
3. **How long was the foothold live?** Cross-reference against access/auth
   logs and this box's own `journalctl` history for the service — this bounds
   which webhook deliveries, connect-flow tokens, and order data were
   plausibly exposed, not just theoretically reachable.
4. **Was the backup chain itself touched?** If `scripts/backup-database.sh`'s
   destination directory was reachable from the compromised box (a local
   `--retain-days`-pruned directory on the same disk, rather than shipped
   off-box), backups made during the exposure window carry the same
   key-material exposure as the live database and must not be treated as a
   "clean" recovery point without the same rotation treatment in §2.

## 4. Merchant notification

- Notify every tenant identified as affected in §3.1 — err toward inclusion
  when the foothold's scope is still uncertain (§3.2 unresolved), since
  under-notifying a genuinely affected merchant is worse than over-notifying
  a merchant whose exposure turns out to have been theoretical.
- State plainly, in these terms, not softened and not exaggerated: **no funds
  were or could be at risk** (watch-only, §6.1 — this is a factual claim
  about the architecture, safe to make with confidence), but **the privacy of
  which incoming transactions belong to your store may have been exposed**
  to whoever had the foothold, for the duration in §3.3.
- Give the merchant a concrete, actionable next step: work with them (or
  point them to Monero tooling/documentation, since generating a new wallet
  is outside this system's own scope per §3 non-goals) to move to a new
  wallet/subaddress tree if they want the *privacy* property restored going
  forward — being explicit that the old addresses and their existing balances
  remain completely safe to spend from whenever they choose; there is no
  urgency on funds, only on future privacy.
- If any jurisdiction's data-breach notification law applies to the exposed
  metadata (§1's order-metadata row) independent of the funds question, that
  determination sits with whoever handles compliance for the operator running
  this box — this runbook does not attempt to give legal advice, only the
  technical facts (what was exposed, to whom, for how long) that determination
  needs as input.

## 5. Recovery checklist

- [ ] Clean box provisioned, service restored from a verified pre-compromise
      backup (WBS 2.3.1 procedure)
- [ ] All `secret_token`s and webhook signing secrets rotated (§2.2)
- [ ] All outstanding connect-flow tokens treated as burned (§2.2)
- [ ] Operator/admin access credentials to the box rotated (§2.2)
- [ ] Scope determined and documented (§3)
- [ ] Affected merchants notified (§4)
- [ ] Compromised box's disk/backups either forensically preserved or
      securely wiped — never left half-preserved-half-reused
- [ ] Post-incident review: how the foothold was gained, and whether it
      changes the WBS 2.2 (SEV-SNP) prioritization — a real, observed
      compromise is the strongest possible argument for pulling that item
      forward, and this runbook's own §1 table (`socket` backend confers no
      confidentiality against a host-level attacker) is precisely the gap
      2.2 exists to close

## 6. Test: tabletop walkthrough

This step is explicitly not code-testable per the WBS brief. The closest
equivalent is a tabletop walkthrough: pick a concrete, specific scenario (e.g.
"an attacker gets a reverse shell via an unrelated service on the same box and
has root for an estimated six hours before detection"), and walk every section
above against it by hand — confirm each step names a concrete command or
concrete decision-maker, not a vague aspiration. Redo this walkthrough whenever
the architecture changes underneath it (most importantly: once WBS 2.2 ships,
§1's `socket` backend row and this document's framing of what a host
compromise exposes both need re-checking, not just this file's existence).
