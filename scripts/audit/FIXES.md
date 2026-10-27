# Fix plan

Each confirmed finding's fix, and the test that protects it if one earns its place. `construction` fixes make the bug impossible and add no test; `construction+test` is kept for critical properties; tests extend existing ones wherever they can.

| Approach | Findings |
|---|---|
| new_test | 13 |
| extend_test | 3 |
| construction+test | 3 |
| no_test | 1 |

## 1 delivery: fan-out per target (top: high, 4 findings)

### #13 Every store sale is pushed to each till's one Live Activity, so a till's island shows another till's sale, its alert and its payment code
`crates/monokulo/src/db/push.rs:528` · high · **new_test** · effort M

**Fix:** queue_sale_pushes pushes a live-activity target only the orders its own app made, for every event kind (encrypted targets keep the store-wide fan-out of decision 76); waitingCount counts the target's own app's waiting orders; the delivery worker sends one target's due pushes one after another in queue order (grouped per target, flatten_unordered across targets) so the last push to land on an island is the newest.

**Test:** `crates/monokulo/src/push/pipeline_tests.rs`: Two apps X and Y in one store, each with a live-activity target with include_payment_uri and an encrypted one: Y's order (waiting, seen, partial, paid, pending) never reaches X's island, still reaches X's encrypted target, and every later phase reaches Y's island with Y's own count; over_http, one target's queued pushes are never in flight at once while another target's go beside them.

**Why:** The island is one per till; another till's sale or its payment code on it is a money and handover risk. The store-wide island the design hints at would need an order identity in the clear and a newest-sale election; the till's own sales are what it shows. Queue order per target closes the out-of-order landing for a till's own overlapping sales.

### #15 One store on a shared monokulo can spend the instance's whole relay sender budget, stopping every other store's pushes
`crates/monokulo/src/db/push.rs:514` · high · **extend_test** · effort S

**Fix:** Collapse a store's targets sharing an endpoint onto the most recently registered one that wants the event: push_targets.registered_at_utc (added in 0053, set on every registration, insert or in place), store_push_targets newest-registered first.

**Test:** `crates/monokulo/src/push/pipeline_tests.rs`: targets_that_share_an_endpoint_are_pushed_once_per_event: the newest registration gets the push, older same-endpoint targets none; re-registering an older target in place makes it the one pushed.

**Why:** Oldest-wins pushed a stale target (old keys) after a re-pair with the same relay handle; the live install heard nothing and nothing failed.

### #6 After sale.partial the Live Activity's paymentURI still asks for the full amount
`crates/monokulo/src/push/payload.rs:92` · medium · **extend_test** · effort S

**Fix:** LiveActivityState::of attaches paymentURI only for SaleKind::Waiting: sale.partial keeps phase waiting but no code, since the order's full amount is no longer what is owed and the engine's event doesn't say what is. Decision 74 is revised in a new decision; the apps guide says so.

**Test:** `crates/monokulo/src/push/payload.rs`: The phases table has Partial's paymentURI false, and a partial state's JSON has no paymentURI while Waiting's keeps it.

**Why:** A code asking for the full price after a part payment makes the customer overpay; no code is the only safe one without the amount due.

### #22 One Topic per order across every target links all of a store's devices for the relay, Apple and Google
`crates/monokulo/src/push/policy.rs:62` · medium · **new_test** · effort S

**Fix:** policy::topic takes the target: the first 32 hex digits of SHA-256(target id || 0x00 || order id), computed per target inside queue_sale_pushes; supersession stays keyed on (push_target_id, topic). Docs in policy.rs and the apps guide say the topic is per target.

**Test:** `crates/monokulo/src/push/pipeline_tests.rs and push/policy.rs`: Two targets of two apps queue one sale event with different topics, a later event of the same order supersedes only each target's own unsent push, a second order gives each target a new topic; the policy unit test checks the topic depends on both target and order.

**Why:** RFC 8030 collapses per subscription, so a shared value adds nothing but an exact device-to-store link for the relay, APNs, FCM and distributors.

## 2 delivery: reads, identities, times, test names (top: medium, 4 findings)

### #1 A push target row this build can't read stops every store's webhooks
`crates/monokulo/src/db/push.rs:527` · medium · **new_test** · effort S

**Fix:** Read a store's or an app's push targets leniently: store_push_targets (queueing, both the logged-event and the sale.waiting paths) and list_push_targets (GET /api/v1/push-targets) leave out a row target_from_row can't read and log it with its id, as due_push_deliveries gives up an unreadable push, so one drifted row costs only its own pushes, never the event's webhooks, the log position or the app's other targets. Registering the same client_id again rewrites the row and heals it.

**Test:** `crates/monokulo/src/push/pipeline_tests.rs (edge and failure cases; the world helper there has the webhook, the app orders and the targets)`: a store with a webhook and two targets, one whose events column holds an unknown kind (raw SQL): an order event queues its webhook and the readable target's push, the position moves, the next event is queued too, another app's new order queues sale.waiting, GET /api/v1/push-targets lists the readable target, and re-registering the bad one's client_id heals it

**Why:** The subscriber already refuses to stall the global log on one deterministic per-event failure; the push side must not reintroduce a stall for every store's webhooks. Leniency at the read keeps the transaction whole (no savepoint juggling) and covers the API list too.

### #5 A 404/410 from a target's old endpoint deletes the subscription it was just re-registered with
`crates/monokulo/src/db/push.rs:737` · medium · **new_test** · effort S

**Fix:** A due push carries the registration of its target it was read with (push_targets.registered_seq, bumped on every registration, in place included). The worker's target-side writes are conditional on it: delete_dead_push_target(id, registered_seq) and record_push_attempt's failure count, limit delete and success reset only touch the target while it still holds that registration. The push's own row is recorded as before, so a push sent to a since-replaced subscription is tried again at the new one.

**Test:** `crates/monokulo/src/push/pipeline_tests.rs (over HTTP)`: a target with a due push to a stub held before answering 410 (then 503): the same client_id is re-registered with a new endpoint while the push is in flight; after the tick the target still exists with the new endpoint and failures 0, and its push is then delivered to the new endpoint

**Why:** registered_seq already identifies a registration (decision 145) and needs no migration; it catches a re-registration with the same endpoint and new keys too, where comparing endpoints would not.

### #7 A push's TTL starts when the event is queued, not when it happened, so a backlog is pushed as fresh
`crates/monokulo/src/db/push.rs:537` · medium · **new_test** · effort S

**Fix:** queue_sale_pushes anchors a push's TTL at the sale event's own time (SaleEvent.at: the engine's created_at for logged events, the order's created_at for sale.waiting), capped at now so an engine clock ahead can't lengthen it: expires_at = event_at + policy::ttl(kind, order.expires_at, event_at, default), MIN_TTL relative to the event. A push whose TTL has already ended at queueing isn't queued (the order's last_sale_event is still recorded and an older unsent push about the order is still superseded).

**Test:** `crates/monokulo/src/push/pipeline_tests.rs (queueing)`: sale.seen and sale.paid events created two hours before the subscriber's clock queue no push and supersede the order's waiting one, still record the order's latest event; the same events created now are queued with today's TTL; a payment seen 30 s after the sale expired gets its minute from the event's time

**Why:** The design bounds how stale a push may be when it lands; anchored at read time, a backlog after downtime or a reconnect was pushed as fresh, replaying settled sales on tills and Live Activities.

### #19 Three push worker tests send real HTTP pushes to a loopback stub but sit under 'in process' without over_http in their names
`crates/monokulo/src/push/pipeline_tests.rs:649` · low · **no_test** · effort S

**Fix:** an_unreadable_push_is_given_up_and_the_others_are_sent needs a push to land, so it moves below the Over HTTP banner as ..._over_http. a_push_with_no_attempts_left_is_given_up_without_a_request and old_pushes_are_pruned_after_push_keep_days become in-process: their attempts are recorded with the live Db methods (record_push_attempt, give_up_push) instead of being made against a stub, so no socket is opened and they stay in the in-process section.

**Why:** Naming-only finding; decision 26 says give-up and pruning are tested in-process, so making the two in-process honours the decision rather than renaming them.

## 3 relay: budgets, secrets, handles, abuse (top: medium, 4 findings)

### #8 Registration limit reads only the first X-Forwarded-For line, so a client behind a line-adding proxy names its own address
`crates/push-relay/src/routes.rs:71` · medium · **construction+test** · effort S

**Fix:** abuse::identity::client_address takes the request's HeaderMap and joins every X-Forwarded-For line itself (a non-text line stops the walk), so no caller can read only the first line; monokulo's anonymous_identity and the relay's register both call it, and the Bell takes it at its next pin.

**Test:** `crates/abuse/src/identity.rs tests; crates/push-relay/tests/relay.rs beside the trusted-proxy test`: identity: two lines, an invented first and the real second, give the real address; a non-text line ends the walk. relay: limit 2, three registrations each with a different invented first X-Forwarded-For line and the same real second line; the third is 429.

**Why:** One helper taking the HeaderMap makes the one-line read impossible to write again, in all three servers.

### #9 Unauthenticated registrations of made-up tokens each keep a row for the full 90-day TTL, so one address can grow the handles file by tens of MB a day
`crates/push-relay/src/handles.rs:290` · medium · **new_test** · effort M

**Fix:** A handle that has never had a push delivered lives handles.unused_ttl_days (new setting, default 7, at most ttl_days) from its registration; the first delivered push marks it delivered and from then on it lives ttl_days from its last registration or delivery. Device tokens are capped per platform at what the platforms issue with room to grow (APNs 200 hex digits, FCM 512 characters).

**Test:** `crates/push-relay/src/handles.rs tests; settings tests`: A registered, never-delivered handle is unknown to find and removed by sweep after the unused TTL; one touched after a delivered push lives ttl_days from the touch, and registering it again keeps ttl_days. Tokens longer than the platform cap are refused. unused_ttl_days above ttl_days stops the relay.

**Why:** Unauthenticated made-up tokens otherwise hold a row for 90 days; real tills re-register on launch and get the long life once a push lands.

### #14 The relay's handles file links every handle of one device by its plaintext token, though the code and design say nothing links them
`crates/push-relay/src/handles.rs:1` · medium · **new_test** · effort L

**Fix:** The handles file is made unlinkable at rest: a handle's id is derived from what it was registered for (SHA-256 over platform, app, token and sender key, keyed to the relay's domain), the row is keyed by SHA-256 of the id, the token and sender key are sealed with AES-256-GCM under a key HKDF-derived from the id, and created_at/last_used_at are dropped and expiry rounded to the hour, so two rows of one device share no value and a row is readable only with its endpoint in hand. Logs name a handle by a short tag of its row key, not its id. Docs (module doc, migration, design, docs page) say what the operator still sees.

**Test:** `crates/push-relay/src/handles.rs tests`: Register one token for two senders, open the SQLite file directly and assert no column value is shared between the two rows except platform/app, and neither the token nor either key appears in the file's bytes; find still answers both handles; registering again finds the same handle.

**Why:** The file was promised to hold nothing linking a device's handles; a backup or moved file linked them by plaintext token.

### #17 A service-account key given as a JSON string puts the whole FCM private key in the start-up error and log
`crates/push-relay/src/fcm.rs:89` · medium · **new_test** · effort S

**Fix:** AccountError::Json keeps serde_json's category, line and column only (as the Bell's BodyError), never its message; a top-level JSON string is named as a key file encoded twice.

**Test:** `crates/push-relay/src/fcm.rs tests`: ServiceAccount::parse of the fixture key file encoded once more as a JSON string fails with a Display holding neither BEGIN PRIVATE KEY nor any line of the fixture key, and saying it is encoded twice; the settings error for fcm.credentials holds none of it either.

**Why:** serde_json quotes a refused string value in full; only the position and kind are safe to print.

## 4 relay: shutdown, push-to-start, providers (top: medium, 4 findings)

### #10 The embedded relay takes SIGTERM/Ctrl-C during monokulo's start-up, so monokulo misses it and never shuts down gracefully
`crates/monokulo/src/relay.rs:117` · medium · **construction+test** · effort M

**Fix:** service_runtime::shutdown::signal() becomes private; the one public way to hear SIGTERM/Ctrl-C is shutdown::on_signal() -> CancellationToken, which installs both handlers synchronously and cancels the token from one task. monokulo's run() makes it before relay::start and uses it for the relay's serve, the main server's graceful shutdown, the wait, and as the event streams' `stopping`; relay::start takes the token. The standalone push-relay and the engine binary get the same single token.

**Test:** `crates/service-runtime/src/shutdown.rs (unit, raising SIGTERM at the test process) and crates/monokulo/src/relay.rs (relay feature)`: A SIGTERM raised before a part asks for the stop is still seen when it does (the old shape, a second signal() made after the signal, never resolves); an embedded relay started with a stop already given stops serving and Running::stop returns at once.

**Why:** Two listeners for one signal is the bug's shape; one token per process, level-triggered, makes a missed signal impossible by construction, on every binary that had the shape.

### #18 The relay's apns-pushtostart platform has no sender: monokulo can only send LiveActivityState, so an opted-in push-to-start target fails every push and is deleted
`crates/push-relay/src/apns.rs:135` · medium · **new_test** · effort S

**Fix:** Remove apns-pushtostart from the relay: Platform::ApnsPushToStart, Start, live_activity_start, the 'apns-pushtostart' CHECK value (0002 is unreleased, edited in place), the docs page rows and the stand-in test; a registration naming it is refused 400 invalid-request and stores nothing. Decision logged; push-to-start comes back with a monokulo target kind that writes its shape.

**Test:** `crates/push-relay/tests/relay.rs registrations_the_relay_cannot_serve_are_refused_with_their_problem`: Registering platform apns-pushtostart answers 400 invalid-request and no handle is stored.

**Why:** No sender writes the Start shape; keeping the platform mints endpoints nothing can feed and whose targets get deleted after 20 refusals. Removing dead code is the house rule until the sender exists.

### #11 A request whose `now` is a second behind the one that just re-signed signs a second APNs token at once
`crates/push-relay/src/apns.rs:359` · low · **extend_test** · effort S

**Fix:** Apns::token reads the clock itself, under its lock (Apns::send passes service_runtime::time::now_unix, no longer the request's arrival time), and reuses the token while issued_at - CLOCK_SKEW (60 s) <= now < issued_at + TOKEN_REUSE, so only a real clock step back re-signs.

**Test:** `crates/push-relay/src/apns.rs a_provider_token_is_reused_for_50_minutes_then_renewed`: token at NOW then at NOW - 1 returns the same JWT; NOW - 3600 still signs anew.

**Why:** The root cause is a time taken before the body read and lookups; reading it under the lock orders it, and a minute's tolerance covers clock slew.

### #12 FCM pushes queue one by one behind a token exchange that can outlast every sender's timeout, so a slow token endpoint becomes timeouts and repeated pushes instead of the relay's 503
`crates/push-relay/src/fcm.rs:242` · low · **new_test** · effort M

**Fix:** Fcm keeps a single-flight token state that also remembers a failed exchange for FAILURE_HOLD (30 s, the default Retry-After), answering 503 at once with the remaining wait; each FCM push (lock wait, token exchange and send) has one budget of server.upstream_timeout_ms, the exchange timed out inside the lock so its failure is remembered. server.upstream_timeout_ms default lowered 10000 -> 4000, below monokulo's push.delivery_timeout_ms (5000) and the Bell's bell.timeout_ms (10000), and its description says it must stay below its senders'.

**Test:** `crates/push-relay/tests/relay_over_http.rs`: A token endpoint that accepts and never answers: ten pushes at once each get 503 with Retry-After within about one upstream timeout, only one exchange is attempted, and a push right after gets 503 at once without another exchange.

**Why:** Bounding the whole attempt below the sender's timeout means the sender always gets the relay's 503 rather than its own timeout, and caching the failure stops queued exchanges multiplying the wait.

## 5a Bell-only (top: low, 3 findings)

### #3 Every payment.updated of an already-COMPLETED payment dings again, refunds included
`monokular/crates/monokular-bell/src/square.rs:163` · low · **new_test** · effort S

**Fix:** read_notification reads the payment's id and whether refunded_money is present (never its value); a payment carrying refunded_money is Ignored. Notification::Completed carries the PaymentId, and a bounded in-memory set of payment ids already rung (10 minutes, 10,000 ids, oldest dropped first) makes the webhook route ring each payment once.

**Test:** `crates/monokular-bell/tests/bell_over_http.rs; unit tests in square.rs and dings.rs`: payment.created COMPLETED for P, payment.updated COMPLETED for P, payment.updated COMPLETED for P with refunded_money: exactly one ding; a COMPLETED event for Q then dings again. Unit: the set forgets after its TTL and holds at most its cap.

**Why:** The recommended fix: one ding per payment, refunds never ding. In memory only, so the Bell still keeps nothing of a webhook on disk; a restart costs one extra ding at most, which design section 3 accepts.

### #4 location_id 'main' passes the ID check but always comes back 503 square-unavailable
`monokular/crates/monokular-bell/src/square.rs:358` · low · **new_test** · effort S

**Fix:** merchant_of answers a 200 for another location than the one asked (Square's 'main' alias, or any other) with LocationError::NotById, which the route answers 400 invalid-request saying the location must be given by its own ID; nothing is stored. The answered id is not stored instead.

**Test:** `crates/monokular-bell/tests/bell_over_http.rs`: The stand-in Square answers GET /v2/locations/main with the merchant's main location's real id; registering with location_id main is 400 invalid-request, not 503, and no row is stored. The existing 'another location answered' case moves from 503 to 400.

**Why:** A mismatched id is a permanent client error, so the till must not be told to retry; refusing 'main' by name alone would leave any other alias at 503.

### #16 Anyone who knows a till's endpoint can move its Bell registration to their own merchant, swap its keys and learn its id
`monokular/crates/monokular-bell/src/registrations.rs:162` · low · **new_test** · effort S

**Fix:** RegistrationStore::register reads the endpoint's existing row in its immediate transaction and takes the registration only when the merchant is the same, or when the keys given are the ones stored (the auth secret, compared in constant time, proves the caller holds the subscription). Otherwise RegisterError::Taken, answered 409 endpoint-registered, with no id; the row, its keys and its failures are untouched. docs/bell.md, design.md and decisions/bell.md say so.

**Test:** `crates/monokular-bell/tests/bell_over_http.rs (and a store unit test in registrations.rs)`: Register endpoint E for merchant M1; register E with M2's token and fresh keys: 409 endpoint-registered, no registration_id in the answer, the row still M1's; a payment at M1's location still dings E decryptable with M1's keys. Registering E for M2 with E's own keys moves it (the till signed in to another account).

**Why:** Root cause is ON CONFLICT DO UPDATE trusting the endpoint alone. Same-merchant moves stay (reinstall, location change); a merchant change needs proof of the subscription, which a leaked endpoint doesn't give, so a till that switched account without unregistering isn't stuck.

## 5b Bell: shared helpers and re-pin (top: low, 1 findings)

### #2 The Bell's SSRF guard is a hand copy of monokulo's, already drifting
`monokular/crates/monokular-bell/src/outbound.rs:40` · low · **construction+test** · effort S

**Fix:** Add the outbound crate (push-relay a0ff005f0, already the pin) as a Bell git dependency; build the ding client with outbound::client_builder(reach).user_agent(USER_AGENT).timeout(timeout).build(); call outbound::check_host at registration and at each ding; Reach comes from outbound; delete crates/monokular-bell/src/outbound.rs with its copied is_private, embedded_ipv4, CheckingResolver, check_host and their tests.

**Test:** `crates/monokular-bell/tests/bell.rs and bell_over_http.rs (existing private/localhost refusals), outbound crate table test (union lists, in monokulo)`: Existing private-endpoint and localhost registration/ding refusals keep passing on the shared rule; add an onion endpoint case to the Bell's refusal test only if push::Endpoint lets one through (it doesn't, so the shared check_host's onion refusal is covered in outbound). The table test of the union lives in the shared crate.

**Why:** One rule in one crate removes the drift by construction: the Bell can no longer hold its own copy.

## Commits and pull requests

Every confirmed finding is `fixed` in the database (none `confirmed`), 2026-10-28. The fixes sit on the draft branches, one commit per finding or tight group, each naming its finding; `push-crate` (#245) and `push-targets` (#246) needed no change. Line numbers in `REPORT.md` refer to the audited commits (monokulo `3304e560b`, monokular `99a0c3b`).

| PR | Branch | Fix commits |
|---|---|---|
| monokulo [#247](https://github.com/OceanEilonwy/monokulo/pull/247) | `push-delivery` | #13 `fe61311f7`, `58c2c8b48` · #22 `c88351bef` · #6 `f96f451aa` · #15 `39e0561a1`, `9e1d588a6` · #1 `783de11a2` · #7 `07ed7df1b` · #5 `e17d05d5c` · #19 `72a1fd90f` |
| monokulo [#248](https://github.com/OceanEilonwy/monokulo/pull/248) | `push-relay` (merges `push-delivery` at `444647770` and `de50b8001`) | #8 `46e69cd85` · #17 `1a84c56de` · #15 `2c0d68c2c` · #14 `d8a669bef`, `31c3cab9d` · #9 `d8a669bef` · #2 `f7bfd3276` · #10 `2aea6dffb`, `6f167df3b` · #18 `cbc1aaea8`, `a0ff005f0` · #11 `9be7c4f42` · #12 `5aeb44d88` |
| monokular [#14](https://github.com/OceanEilonwy/monokular/pull/14) | `monokular-bell` | #4 `4f98d4b` · #16 `d0debba` · #3 `7dcd133` · #8 `5516491` · #10, #18 `ddeda82` · #2 `c778067` · #15 `1e84895` · design brought up to #13, #22, #6, #14, #9: `c200779`, `cd0a8d8`, `99ce8cf` |

The reports: monokulo `docs/reports/bell-and-relay.md` section 10 (each finding's fix, test and commit, and the gates run on the final head) and monokular `docs/reports/bell.md` "Audit fixes". The decisions: monokulo `docs/decisions/push.md` 139 to 159, monokular `docs/decisions/bell.md` 36 to 42.

Fix groups were named inconsistently by their fixers when the plans were recorded (`1-delivery-fanout`, `1`, `group3-relay`, `4`, ...); they were renamed in the database to the six groups of the fix order before this report, so the groups above match `REPORT.md`'s.

The reports themselves: monokulo `aed855ab8` (the head of `push-relay`, also carrying the fuzz lock fix `6f167df3b`) and monokular `45be0f2`, which re-pins the Bell's monokulo crates at `aed855ab8`.
