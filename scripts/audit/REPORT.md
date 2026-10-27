# Push audit: the Bell and relay stack

Audit of the code that the Bell and relay stack added or changed. **Status (2026-10-28): every confirmed finding is fixed** on the draft branches, each with its regression test; see `FIXES.md` for the fix, test, commit and PR of each, and section 10 of monokulo's `docs/reports/bell-and-relay.md` (on `push-relay`). The stack's PRs stay unmerged until the owner reviews them. Read this beside the team's build report. The raw generated report is `target/code-audit-push/report.md`, the database is `target/code-audit-push/audit.db`, and the method is in `DECISIONS.md`.

## Scope

| | |
|---|---|
| monokulo | `8c7d902e6..3304e560b`, 40 commits, PRs #245 `push-crate`, #246 `push-targets`, #247 `push-delivery`, #248 `push-relay` (stacked in that order) |
| monokular | `ca5e54018..99a0c3b`, 8 commits, PR #14 `monokular-bell`, checked out under `monokular/` |
| Diff | monokulo: 100 files, +20,979 / -423 (Rust: 80 files, +18,817 / -411). monokular: 25 files, +7,221 / -108 (Rust: 14 files, +5,357) |
| Audited files | 92 Rust files (14 in the Bell). Changed files are sized by their changed lines: 24,583 lines, made up of 15,611 non-test, 8,892 test and 81 tooling |
| Modules | 10: push-crypto, push-send, monokulo-push-targets, monokulo-push-delivery, relay-core, relay-providers, relay-settings-embedding, bell, bell-settings, support-tooling |
| Jobs | 49, all done: 16 module reviews, 11 cross-cutting themes, 22 verifications |
| Agents | 5 (a1 to a5). Each finding was verified by an agent other than its reporter |
| Tool signals | 67 clippy hints, triaged inside the reviews. None became a finding |

**Themes:** web-push crypto, privacy, authz and abuse, delivery semantics, concurrency, type safety, dead code and wiring, error handling, house rules, test quality, design conformance.

## Counts

22 findings were recorded:

- **20 confirmed:** 0 critical, 2 high, 11 medium, 7 low.
- **1 rejected.**
- **1 duplicate.**

By category, the confirmed findings are 17 bugs, 1 security, 1 simplify and 1 convention.

The two high findings were read again against the code at 3304e560b for this report, and both hold.

## Confirmed findings

Each finding is grouped under the module that owns its file. The theme that found it, where there is one, is in brackets.

### High

#### monokulo-push-targets

**#13 Each till's single Live Activity is sent every sale in the store, so the island can show another till's sale, alert and payment code** (delivery-semantics)
`crates/monokulo/src/db/push.rs:528-561`, with `push/payload.rs:52-121`

- **Problem:**
  - `queue_sale_pushes` sends to every target in the store that wants the event. Only `sale.waiting` from `queue_waiting_push` passes `except`, and that only skips the app that made the order.
  - Each push gets a per-order Topic. `LiveActivityState` carries no order id, only the optional merchant label `sale`.
  - iOS applies a Live Activity's content-state with no app code, so whichever push lands last replaces the whole island.
  - The worker uses `buffer_unordered`, so order across orders is not kept.
  - The design's "newest plus +N more" is not implemented anywhere.
- **Failure:**
  - Two iPhone tills, X and Y, with "Show code in island" on. X rings up sale A for 24.50 USD.
  - Y rings up sale B for 10.00 USD, and X's target receives B's `waiting` state with B's `paymentURI`.
  - X's customer pays B's address and amount. B goes Paid and Y hands over B's goods, while A stays unpaid.
  - Without the code in the island, B's `sale.paid` alert still lands on X while A is waiting.
- **Fix:**
  - Push a live-activity target only its own app's orders, for every event kind, and never send another app's `paymentURI`.
  - If a store-wide island is wanted, have the state name the sale, push only the newest waiting sale, and send in queue order for each target.
- **Test:** In `pipeline_tests`, two apps in one store, each with a live-activity target and `include_payment_uri` on. Y's order must not reach X's target in any phase, and must still reach Y's own target in every later phase.
- **Re-check:** The filter is at lines 527-532. `queue_from_event` passes `None` for `except` (line 487). `paymentURI` is gated only on the phase and the opt-in (`payload.rs:113-114`). Holds.

**#15 One store on a shared monokulo can spend the instance's whole relay sender budget** (authz-and-abuse)
`crates/monokulo/src/db/push.rs:514-590`, with `crates/push-relay/src/limits.rs:54-66`

- **Problem:**
  - All stores on an instance sign with one VAPID key. The relay's only fairness limit is `limits.per_sender_per_minute`, 600 by default.
  - Monokulo has no per-store budget, and nothing stops 8 targets (`MAX_TARGETS_PER_APP`) from sharing one relay endpoint.
  - `Limits::push` charges the sender before the handle, and `check_with_retry` increments the count when a check passes. So pushes the handle limit (60) refuses still use up the sender's budget.
  - Monokulo retries 429s without counting them, so the starved stores' pushes expire.
  - The Bell has the same shape and drops 429 dings.
- **Failure:**
  - On www.monokulo.com, using relay.monokulo.com, app B registers 8 targets on one handle.
  - App A makes about 75 orders a minute. That is about 600 pushes a minute, 540 refused at the handle but all charged to the sender.
  - Every other store's `sale.paid` and Live Activity pushes get 429 and expire.
  - A busy instance with a few hundred tills reaches 600 a minute with no abuse.
- **Fix:**
  - **Monokulo:** a per-store push budget below the per-sender limit, and one push per event for targets that share an endpoint.
  - **Relay:** check the handle before charging the sender, or refund the sender. Key fairness by (sender, handle) too.
  - **Docs:** size `per_sender_per_minute` for a multi-store instance and for the Bell.
- **Test:**
  - In `pipeline_tests`, store A floods 8 targets that share an endpoint, and store B's `sale.paid` still goes out on its first attempt.
  - In the relay's limits tests, a push refused at the handle leaves the sender's budget unspent.
- **Re-check:**
  - `limits.rs:59-64` checks the sender first, and its doc comment says that is deliberate.
  - The limiter increments on pass at `rate-limit/src/lib.rs:133`.
  - The default of 600 is at `settings.rs:88-95`.
  - Holds. Swapping the check order alone is not enough, because the budget can still be drained across several handles.

### Medium

#### monokulo-push-targets

**#1 One push target row this build can't read stops every store's webhooks** (`crates/monokulo/src/db/push.rs:527-532`)

- **Problem:**
  - Pushes are queued inside the webhook transaction (`db/webhooks.rs:383`, with `?`).
  - `store_push_targets` fails the whole read on one row it can't parse: an unknown `SaleKind`, an `Endpoint::parse` that has since been tightened, or a kind and keys that don't match.
  - The subscriber then retries the same position every minute. `list_push_targets` fails the same way.
- **Failure:** After a rollback or a rules change, every store's webhooks and pushes stall until the row is deleted by hand. Decision 64 records an earlier drift of this kind.
- **Fix:** Read targets leniently when queueing, skipping and logging a bad row, or use a savepoint whose error is logged and swallowed.
- **Test:** In `webhooks/tests.rs`, a raw-SQL row with an unknown kind. The webhook must still be queued, the position must move on, and the next event must be handled.

**#5 A 404 or 410 from a target's old endpoint deletes its re-registered subscription** (`db/push.rs:737-744`, with 700-716)

- **Problem:** The worker takes a snapshot of each push with its endpoint. Re-registering with the same `client_id` keeps the id and swaps the endpoint. Outcomes are then applied by id alone.
- **Failure:** The subscription rotates while a push is in flight. The old endpoint answers 410, and the fresh target is deleted.
- **Fix:** `DELETE ... WHERE id = ?1 AND endpoint = ?2`, condition the failure counter and `last_ok_at` on the attempted endpoint, or add a generation column.
- **Test:** A 410 recorded after re-registration leaves the target with the new endpoint and its failure count at 0.

**#7 A push's TTL starts at queue time, not event time** (`db/push.rs:537-540`)

- **Problem:** The TTL and `expires_at` come from the subscriber's clock, not `LoggedEvent.created_at`. `policy::ttl` clamps to at least 60 s, or the full default, from now.
- **Failure:** After two hours of downtime, the whole backlog is pushed as fresh: old alerts and island states replay on the tills.
- **Fix:** Anchor the TTL at the event's time (`created_at`, or `order.created_at` for `waiting`). Skip pushes that have already expired, but still record `last_sale_event`.
- **Test:** Events created 2 hours ago are not sent, while the same events created now are.

#### monokulo-push-delivery

**#22 One Topic per order, shared by every target, links a store's devices for the relay, Apple and Google** (design-conformance, security; `push/policy.rs:62-71`, with `db/push.rs:537`)

- **Problem:**
  - The topic is computed once per order, outside the loop over targets.
  - The relay passes it on as `apns-collapse-id` and FCM's `collapse_key`.
  - This contradicts design section 5 ("tells the relay nothing", "no device-to-store link").
  - RFC 8030 section 5.4 collapses only within one subscription, so sharing gains nothing.
- **Failure:** One sale shows the relay, APNs, FCM or a distributor exactly which devices belong to one store, even with one VAPID key across thousands of stores.
- **Fix:** A per-target topic: the first 32 hex digits of SHA-256(target id ‖ 0x00 ‖ order id), computed inside the loop. Update the design, the apps guide and the docs in `policy.rs`.
- **Test:** Two targets get different topics, supersession stays within each target, and a new order gives each target a new topic.

**#6 After `sale.partial`, `paymentURI` asks for the full amount** (`push/payload.rs:92-121`)

- **Problem:** Partial maps to Waiting, and the URI is built from the order's full piconero amount. The checkout asks for the amount due. Decision 74 is the defect.
- **Failure:** The customer pays 0.3 of 0.5 XMR, scans the island, and pays 0.8 in total.
- **Fix:** No `paymentURI` on Partial, or have the partial event carry the amount due.
- **Test:** `LiveActivityState::of(Partial, …, true).payment_uri` is `None`, while `Waiting` keeps it.

#### relay-core

**#14 The handles file links one device's handles by plaintext token** (privacy; `handles.rs:1-10` claims, 302-315 write, migration 0001)

- **Problem:** `token` and `vapid_public_key` are stored in the clear, against four claims: the module doc, the migration comment, design.md lines 438 and 596, and the docs page.
- **Failure:** Anyone holding a backup or a moved `push-relay.db` learns each till's monokulo, and whether it uses the Bell.
- **Fix:** Correct the claims everywhere, or make the file unlinkable at rest: an HMAC-derived id stored only as a hash, the token AEAD-encrypted, and the same for the key column.
- **Test:** Two senders' rows for one token share no column value.
- **Note:** Lowered from high, because the operator necessarily sees both values on every request.

**#9 Made-up-token registrations keep rows for 90 days** (`handles.rs:290-331`, with `routes.rs:65-122`)

- **Problem:** Registration is unauthenticated. Any well-formed token up to 1,024 characters with a fresh key makes a new row. The only bound is 30 a minute per address, and the limiter's map is cleared at 40,000 addresses.
- **Failure:** About 100 MB a day from one IPv4 address, and an IPv6 /48 fills the disk in hours.
- **Fix:** A shorter life for handles that have never had a push delivered (a few days), extended on delivery. Cap token length per platform. Optionally cap rows per (app, sender).
- **Test:** An untouched handle is swept after the shorter TTL, while a delivered one lives `ttl_days`.

**#8 Only the first X-Forwarded-For line is read** (`push-relay/src/routes.rs:71-76`, and the Bell's `routes.rs:73-76`)

- **Problem:** A proxy that adds its own line (HAProxy's `option forwardfor`) leaves the client's invented line first. Monokulo's own limiter already joins every line with `get_all`.
- **Failure:** The registration limit is bypassed. In the Bell, each registration also costs a Square request.
- **Fix:** One `HeaderMap` helper in the abuse crate, used by monokulo, the relay and the Bell.
- **Test:** Three registrations with an invented first line and the same real second line, limit 2: the third gets 429.

#### relay-providers

**#18 `apns-pushtostart` has no sender** (dead-code-wiring; `apns.rs:135-145`, 198-202, 255-275)

- **Problem:** Nothing writes the `Start` shape. The path is reached only by tests.
- **Failure:** Pushes to a push-to-start target get 400, and the target is deleted after 20 refusals.
- **Fix:** Remove the platform, or add a third target kind. Until then, refuse the platform at registration.
- **Test:** If kept, the stand-in APNs receives `event: start`. If removed, registration refuses the platform.

**#17 The FCM private key appears in the start-up error** (error-handling; `fcm.rs:89-92`)

- **Problem:** serde_json's message quotes a top-level string in full.
- **Failure:** A key file encoded twice puts the RSA key into journald, container logs and OTel.
- **Fix:** Keep only `classify()`, the line and the column, as the Bell's `BodyError` does.
- **Test:** A key passed as a JSON string gives an error whose `Display` contains no key material.

#### relay-settings-embedding

**#10 The embedded relay takes SIGTERM and Ctrl-C during start-up** (`monokulo/src/relay.rs:117-121`, with `main.rs:176` and 364-366)

- **Problem:** `serve(…, shutdown::signal())` installs the handlers early. Main's listener, created later, marks the earlier signal as seen.
- **Failure:** The relay stops, monokulo keeps running, and systemd SIGKILLs it 90 s later.
- **Fix:** One shared shutdown token (the existing `stopping`), fed by one `signal()` task. The standalone relay has the same shape.
- **Test:** A stop raised during start-up still resolves `run()`'s stop future.

### Low

#### bell

**#3 `payment.updated` for a payment already COMPLETED dings again** (`square.rs:163-182`)

- **Problem:** Refunds and fee updates send `payment.updated` with status COMPLETED, and each one dings.
- **Failure:** Once the iOS extension ships, tills show false "New sale" alerts.
- **Fix:** Deduplicate by payment id for about 10 minutes, and ignore updates that carry `refunded_money`.
- **Test:** Three events for one payment give one ding, and a second payment gives a second.
- **Note:** Lowered from medium. Fix it before iOS ships.

**#4 `location_id` "main" is answered 503 forever** (`square.rs:358-362`)

- **Fix:** Answer a mismatched location with 4xx `Refused`, or refuse "main" up front.
- **Test:** Registering with "main" gets a 4xx and stores nothing.

**#16 A Bell registration can be moved by anyone who knows the endpoint** (authz-and-abuse; `registrations.rs:162-217`)

- **Problem:** `ON CONFLICT (endpoint)` overwrites the merchant, location and keys and returns the existing id. Unregistering needs only that id.
- **Fix:** Allow changes only within the same merchant, or require proof of the subscription. Otherwise answer 409.
- **Test:** A second merchant's registration gets 409, and M1's dings still reach the endpoint.

**#2 The Bell's SSRF guard is a hand copy that has drifted** (simplify; `outbound.rs:40-185`)

- **Problem:** The copies disagree on localhost names and onion hosts.
- **Fix:** One shared guard in a light crate, with a union table test.

#### relay-providers

**#12 FCM token exchange queueing outlasts the senders' timeouts** (concurrency; `fcm.rs:242-307`)

- **Problem:** The mutex is held across a POST of up to 10 s, failures aren't cached, and monokulo gives up after 5 s.
- **Failure:** Pushes time out instead of getting the 503, and pushes are sent twice.
- **Fix:** Cache a failure briefly and answer 503 at once. Keep the upstream timeout below the senders' timeouts.
- **Test:** Ten concurrent pushes all get 503, with one exchange attempted.

**#11 A backward `now` re-signs the APNs token** (`apns.rs:359-382`, with `routes.rs:132-135`)

- **Problem:** `now` is taken before the body is read.
- **Failure:** Two tokens in a second, so APNs answers 429 `TooManyProviderTokenUpdates`.
- **Fix:** Tolerate about 60 s of skew, or read the clock under the lock.
- **Test:** `token(NOW)` and `token(NOW-1)` return the same JWT, and `NOW-3600` re-signs.

#### monokulo-push-delivery

**#19 Three worker tests make loopback HTTP requests without `over_http` in their names** (house-rules; `pipeline_tests.rs:649-830`)

- **Fix:** Rename them and move them under the Over HTTP banner, or make them in-process.

## Rejected and duplicate

| Id | Status | Note |
|---|---|---|
| #20 | rejected (was medium) | `relay.rs:194` has tests that run `prepare` and `start`. Only the shutdown wiring is untested, and #10's regression test covers it |
| #21 | duplicate of #19 | Same tests |

**Considered and not filed:**

- The Bell counting 5xx toward its failure limit (a decision).
- No replay protection for Square webhooks (decision 18).
- The VAPID `sub` naming the instance (push.md 13 and 79).
- At-least-once re-sends after a restart.
- No per-store share of a batch (push.md 84).
- One `apns.endpoint` per relay.
- The double-spend alert text (decision 74).

## What looked solid

- **push-crypto:** RFC 8291 matches Appendix A byte for byte, every message gets a fresh salt and key, secrets are zeroized, and the JWT is pinned to ES256 with `none` and `crit` refused. `aud` and `exp` are checked strictly. No findings.
- **push-send:** follows RFC 8030 headers and answer mapping, reads every Retry-After form, strips URLs from errors, and validates endpoints and keys. No findings.
- **monokulo-push-targets:** scoped by app, idempotent registration in an IMMEDIATE transaction, cascade on disconnect, the VAPID key encrypted.
- **monokulo-push-delivery:** fan-out shares the subscriber's transaction, supersession holds while in flight, past-TTL pushes are dropped, 404 and 410 delete, 429 isn't counted, the outbound move preserves behaviour, and payloads carry no order id.
- **relay-core:** VAPID is checked against each handle's own key, limits are counted after verification, handle ids are unique and random, and nothing sensitive is logged.
- **relay-providers:** APNs headers and JWT, Live Activity payloads, FCM v1 and RFC 7523, and Debug redaction are all right.
- **relay-settings-embedding:** the `[relay]` split is checked on every start, secrets come from the environment only, and invalid values stop the process.
- **bell:** the HMAC is constant-time and matches Square's vector, the token is `Zeroizing`, redirects are off, the host is checked first, the limit is counted before the body is read, the cap uses an IMMEDIATE transaction, and shutdown waits for dings.
- **bell-settings and support-tooling:** no findings.
- **Themes with no new findings:** web-push crypto and type safety.

## Patterns

1. **Store-wide fan-out where the target should be the unit:** #13, #15 and #22, all from `queue_sale_pushes`. Design their fixes together.
2. **Privacy promises that go further than the code:** #14 and #22.
3. **Times taken too early, and identities matched too loosely:** #7, #11 and #10 for times, #5 for identities.
4. **Strict reads in shared paths:** #1.
5. **The same fix needed in several places:** #8, #2 and #15. Shared crates remove the drift.
6. **Paths with no producer, and known edge cases left at the user:** #18 has no producer. #6 and #17 are known edges that were not followed through to the user.

## Recommended fix order

One PR per module, based on the branch that introduced the code. Restack `push-delivery` → `push-relay` after each. Nothing merges into `main` before the owner reviews it.

1. **On `push-delivery` (#247): #13, #22, #15 (monokulo half) and #6.**
2. **On `push-delivery`: #1, #7, #5 and #19.**
3. **On `push-relay` (#248): #15 (relay half), #17, #14, #9 and #8.** The X-Forwarded-For helper goes in the abuse crate.
4. **On `push-relay`: #10, #18, #12 and #11.**
5. **On `monokular-bell` (monokular #14): #16, #3, #4 and #2, plus the Bell's halves of #8 and #15.** Re-pin to the shared helpers once they land.
