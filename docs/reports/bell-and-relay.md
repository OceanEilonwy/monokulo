# Push, the push relay and the Monokular Bell: project report

Written 2026-10-27 by the principal Rust engineer of the five-agent team
(a project planner, a principal Rust engineer, two Rust engineers and a
test engineer) that built Monokulo's push system and the Monokular Bell in
one afternoon while the owner was away. Nothing here was asked of the owner:
every call is numbered in `docs/decisions/push.md` (monokulo, decisions 1 to
138) and `docs/decisions/bell.md` (monokular, 1 to 35), with its reason, so
any of them can be reversed knowingly. This report is the way in: what was
built and where, how to try it, the decisions in brief, where the build left
the design document, the tests and gates, the risks, and what is left.

The design is monokular's `docs/design.md` (§3 wake-ups and the Bell, §5
push, §6 the Live Activity, §8 monokulo's layout), read against monokulo
`main` at `8c7d902e6` and monokular `main` at `abacc01`; the plan that split
the work is `~/Downloads/mokulo-plan-bell-relay.md` on the owner's machine.
`main` did not move in either repository while the work was done.

## 1. In short

A connected app (a till) registers where it wants pushes: a Web Push
subscription, to a UnifiedPush distributor or to the new push relay, whose
endpoints lead to APNs and FCM. Monokulo then pushes each sale event of an
order any of the store's apps made, encrypted to the device (RFC 8291) and
signed with this Monokulo's own VAPID key (RFC 8292), riding the webhooks
pipeline: queued in the event's transaction, sent by a worker that backs
off, supersedes by topic and drops past the TTL. The relay runs inside
monokulo (`relay.enabled`, off by default) or on its own as
`monokulo-push-relay`, holds an operator's Apple and Google keys, and learns
nothing but device tokens. The Monokular Bell, in the monokular repository,
turns a Square application's payment webhooks into Web Push dings for the
tills registered at that location, so a till asks Square for the new
payment within a second or two instead of polling every two seconds.

Everything is server side (plan decision 1): the Android and iOS apps
aren't wired to any of it yet, and nothing is deployed (decision 2). The
contracts the apps code against are written: the apps guide's "6. Get
pushes" (`xtask/src/pages/docs/apps.md`), the relay's page
(`xtask/src/pages/docs/push-relay.md`) and the Bell's `docs/bell.md`.

## 2. What was built and where

### monokulo (OceanEilonwy/monokulo), 38 commits, about 20 000 lines

Four stacked branches, each a package of the plan and a draft PR:

| Branch, PR | Package | What it holds |
| --- | --- | --- |
| `push-crate`, [#245](https://github.com/OceanEilonwy/monokulo/pull/245) onto `main` | P1 | `crates/push`: Web Push as a library with no monokulo types, built on stable. `Endpoint`, `Origin`, `P256dh`, `AuthSecret`, `Subscription`; `Urgency`, `Ttl`, `Topic`, `PlainJson`, `Message`, `Options`; RFC 8291 `encrypt` (fresh salt and key per message; `decrypt` and `ReceiverKeys` behind `test-support`); `jwt::Es256Key`; VAPID (`VapidKey`, `Authorization`, `verify`); `wire::PushRequest::parse` (what a relay reads, exactly what `Sender` writes); `Sender` over the caller's `reqwest::Client`, one token per origin for 11 h, answers sorted into `Sent`/`SendError`. lint.yml checks it on stable. |
| `push-targets`, [#246](https://github.com/OceanEilonwy/monokulo/pull/246) onto `push-crate` | P2 | Migration `0052_push_targets.sql` (`vapid_keys`, `push_targets` with `CHECK`s tying kind to keys); `db/push.rs` (`TargetKind::Encrypted{p256dh,auth} \| LiveActivity{include_payment_uri}`, register as an upsert on `(app_id, client_id)`, 8 per app); `push::Push` holding the VAPID key, made at first start, private scalar encrypted under `Binding::VapidKey`; `/api/v1/push-targets` (GET with `vapid_public_key`, POST `register-push-target`) and `/api/v1/push-targets/{id}` (GET, DELETE `unregister-push-target`), `ak_` keys only; `Problem::TooManyPushTargets`; `SaleKind` as the event name; a macros change so a resource's own forms target the resource; recorded examples for every answer; apps guide "6. Get pushes". |
| `push-delivery`, [#247](https://github.com/OceanEilonwy/monokulo/pull/247) onto `push-targets` | P3 | `crate::outbound` (the webhooks' checking client, shared); `push.*` settings (`PushConfig`, a "Pushes to apps" card); migration `0053_push_deliveries.sql` (`push_deliveries`, `app_orders.last_sale_event`); queueing in `Db::queue_order_event` and from the order API (`sale.waiting` to the other apps); `push::policy` (urgency, TTL, hashed topic), `push::payload` (the Sale JSON; the Live Activity state), `push::delivery` (the supervised worker: batches of 50, 16 in flight, 5/20/80/320 s or `Retry-After`, never past the TTL, 404/410 and 20 counted failures delete the target, pruning); apps guide "What arrives". |
| `push-relay`, [#248](https://github.com/OceanEilonwy/monokulo/pull/248) onto `push-delivery` | P4 | `crates/push-relay` (library and `monokulo-push-relay`): settings declared once with live-settings (`server.*`, `store.*`, `handles.*`, `limits.*`, `apns.*`, `fcm.*`, `logging.*`; secrets `MONOKULO_RELAY_HANDLE_KEY`, `MONOKULO_RELAY_APNS_KEY`, `MONOKULO_RELAY_FCM_CREDENTIALS` from the environment only); `POST /v1/register` and `POST /v1/push/{handle}`; handles in their own SQLite file; APNs over HTTP/2 with a 50-minute ES256 token (alert and Live Activity update; push-to-start, built here, was taken out by the push audit, decision 157), FCM HTTP v1 with an RS256 service-account token; limits per sender, handle and address; problem+json answers; no body, token or endpoint in any log line. In monokulo: the default `relay` feature, `relay.enabled`, `crates/monokulo/src/relay.rs` (reads `[relay.*]` at every start, serves on 8444 when enabled), `--init`/`--help` covering two embedded services, the Dockerfile builds the binary, a docs page `/push-relay/`. Also, from the integration: `push::pipeline_tests::relay_tests`, the whole chain in three tests, and the paid alert's title with its currency. |

Also in the stack: `docs/decisions/push.md` (this build's decision log) and
this report.

### monokular (OceanEilonwy/monokular), 7 commits

| Branch, PR | Package | What it holds |
| --- | --- | --- |
| `monokular-bell`, [#14](https://github.com/OceanEilonwy/monokular/pull/14) onto `main` | P5 | `crates/monokular-bell` (library and `monokular-bell`), on monokulo's `push`, `live-settings`, `sqlite`, `rate-limit`, `abuse` and `service-runtime` as git dependencies pinned to one monokulo commit (the head of `push-relay`). `POST /v1/register` (the Square token used once for `GET /v2/locations/{id}`, whose `merchant_id` proves token and location, then dropped), `DELETE /v1/register/{id}`, `GET /v1/info`, `POST /v1/square/webhook` (HMAC-SHA256 over `public_uri + path + body`, constant time; `type`, `merchant_id`, `location_id`, `status` read and nothing else; `200` at once, then one encrypted `{"wake":"square"}` ding per registration, `Urgency: high`, `TTL: 60`, `Topic: wake`); registrations in its own SQLite file; `bell.*` settings, secrets `MONOKULAR_BELL_SQUARE_WEBHOOK_KEY` and `MONOKULAR_BELL_VAPID_KEY` from the environment only. `tools/fake-square` answers a location and sends signed webhooks; `rust.yml` tests the Bell; `docs/bell.md` (the API), README "Running a Bell", `docs/decisions/bell.md`, `docs/reports/bell.md`, and `docs/design.md` brought up to what was built. |

### Dependencies

New lock entries in monokulo: `p256` 0.14.0 (2026-07-03) and `h2` 0.4.20
(2026-10-06); `base64` 0.23.1 (already in the lock) joins monokulo's
dev-dependencies. Everything else was in the tree. In the Bell, every new
lock entry was checked against crates.io for a release date on or before
2026-10-13 (`tokio` held at 1.53, `rusqlite` at 0.40.2). Not taken, with the
reasons in decisions 3 to 6: `web-push-native`, `jwt-simple`, `gcp_auth`,
`governor`. Everything is rustls and RustCrypto; no OpenSSL, no `ring`.

## 3. How to review

Read the PRs bottom-up: #245 (the crate) → #246 (targets) → #247
(deliveries) → #248 (the relay and the chain test) → monokular #14 (the
Bell). Each PR's diff is its package alone, its body says what it adds and
which checks ran, and `docs/decisions/push.md` has a section per package,
its tests and its review. Merge in the same order; after #245 merges GitHub
retargets #246 onto `main`, and so on. **Merge with merge commits or
rebases, not squashes**: the Bell pins a commit of `push-relay` (its head
when the Bell was last checked; a later commit that changes none of the
crates the Bell takes needs no re-pin), and a squash would leave that
commit unreachable (decision 134). After the
stack is on `main`, move the Bell's pin to `main`'s commit (`rev` in
monokular's `Cargo.toml`, then `cargo update -p push --precise <commit>`),
as decision 22 planned.

Two migrations ship (production note: the owner runs an instance since
2026-10-17). Both add tables only; `0053` adds one nullable column with a
`CHECK` to `app_orders`, where existing rows stay `NULL`. The first start
after the upgrade writes one `vapid_keys` row and reads it back; a wrong
`MONOKULO_ENCRYPTION_KEY` stops the process with a clear message. Nothing is
pushed until an app registers a target; `relay.enabled` is off.

## 4. How to run and try it locally

**Pushes, with a real push service.** Run monokulo as usual and connect an
app from the store dashboard (Connected apps). With the app's `ak_` key, a
UnifiedPush endpoint from ntfy (`https://ntfy.sh/<topic>?up=1`) and a
subscription key pair made with any Web Push library (the keys are the
device's; for a smoke test `push::encrypt::ReceiverKeys::generate()` in a
test prints them with `subscription_keys()`):

```sh
curl -H "Authorization: Bearer ak_…" https://your.monokulo/api/v1/push-targets
curl -H "Authorization: Bearer ak_…" -H 'Content-Type: application/json' \
  -d '{"client_id":"ntfy","kind":"encrypted","endpoint":"https://ntfy.sh/<topic>?up=1",
       "keys":{"p256dh":"BC…","auth":"…"},"events":["sale.paid"]}' \
  https://your.monokulo/api/v1/push-targets
```

Make an order with the app's key (`POST /api/v1/orders`) and pay it: ntfy
shows an `aes128gcm` body arriving for the topic; `push_deliveries` on the
Query page shows the row delivered. Plain `http` endpoints are taken for
this machine only, and pushed to only with `webhooks.allow_private_urls` on.

**The relay, standalone.** `cargo run -p push-relay --bin monokulo-push-relay
-- --init` writes `~/.config/monokulo/relay.toml` with every setting
described; set `server.public_uri`, `handles.apps`, `apns.team_id`,
`apns.key_id`, and run it with `MONOKULO_RELAY_HANDLE_KEY` (`openssl rand
-hex 32`, kept), `MONOKULO_RELAY_APNS_KEY` (the `.p8`) and
`MONOKULO_RELAY_FCM_CREDENTIALS` (the service-account JSON) in the
environment. Inside monokulo: the same keys under `[relay.*]` with
`enabled = true` in `[relay]`; `monokulo --init` writes them, `monokulo
--help` names the secrets. The docs page `xtask/src/pages/docs/push-relay.md`
has the register and push requests. Without Apple or Google keys the whole
chain still runs on one machine, against stand-ins, as a test:

```sh
cargo nextest run -p monokulo --features monokulo/snp -E 'test(relay_tests)'
```

(a relay served on `127.0.0.1:0`, a sale pushed to its handles, the APNs
alert's ciphertext decrypted with the device's keys, the FCM data message
and the Live Activity's update read; 0.2 s).

**The Bell, on one machine.** monokular's README, "Running a Bell": the
stand-in Square (`tools/fake-square/fake_square.py`) sends a signed
`payment.created` to the Bell for each sale rung up at its `/fake/sale`,
and the Bell dings a Web Push endpoint on loopback. Its over-HTTP test
suite does the same with a stub endpoint and decrypts the ding:
`cargo nextest run -p monokular-bell` (71 tests, about a second).

## 5. The decisions, in brief

The logs hold each with its reasoning and alternatives; this is the index.
Numbers are `docs/decisions/push.md`'s unless marked B (monokular's
`docs/decisions/bell.md`).

**The plan (1–28, the planner).** 1 server side only today. 2 nothing
deployed or published; hosts stay placeholders. 3 RFC 8291 and VAPID
written on `p256` 0.14 and the RustCrypto crates in the lock, not
`web-push-native` (a generation behind). 4 no `jwt-simple`: one ES256
signature. 5 no `gcp_auth`: it brings `ring` and finds credentials by
itself. 6 no `governor`: the workspace's `rate-limit`. 7 push targets are
`POST`ed with an idempotent `client_id` on a `GET`-able collection, not
`PUT`. 8 the VAPID public key is read from the collection, not the connect
response. 9 only `ak_` keys have push targets. 10 `events` is the enum
list, no globs. 11 the two target kinds can't mix (Rust enum, SQL `CHECK`).
12 at most 8 targets per app. 13 one VAPID key per instance, private
scalar encrypted; subscription keys in the clear. 14 deliveries keep the
plaintext and encrypt per attempt; supersede by topic; drop past the TTL.
15 backoff 5/20/80/320 s, 5 attempts, dead targets deleted. 16 the
embedded relay's settings under `[relay.*]`/`--relay-…`, not the admin
page. 17 the relay listens on 8444. 18 no `push.enabled`. 19 endpoints
`https` only, never onion, private addresses checked as webhooks' are. 20
no relay unregister route. 21 no provider traits or in-memory stores for
tests. 22 the Bell pins monokulo's crates by git revision. 23 new
dependencies dated on or before 2026-10-13. 24 the Bell proves the
location with `GET /v2/locations/{id}`. 25 the Bell reads four fields of a
webhook and nothing else. 26 real HTTP only where a push must land, named
`over_http`. 27 `monokulo-push-relay` ships in the Docker image. 28 ids
`pt_`, `ph_`, `br_`.

**The `push` crate (29–52).** 29 `MAX_PLAINTEXT` 3993, the RFC's number.
30 `Sender::send` takes an `Endpoint`. 31 `Authorization` holds token and
key; no second `sender_key`. 32 `verify` takes a parsed `Authorization`. 33
the algorithm is never the token's to choose. 34 no endpoint capability
reaches a log (`Debug` prints the origin). 35 `from_pkcs8_pem` comes with
its caller, the relay. 36 `parse` takes `&[u8]`. 37 keys read with or
without padding, written without. 38 what `PushRequest::parse` refuses. 39
the token cache: under 11 h, stale ones forgotten. 40 `request_parts` is
`wire::headers`, private. 41 the push service's body is never read. 42
`Content-Type: application/octet-stream` and `Urgency` always written. 43
CI checks `push` on stable. 44 the 2048 bound is checked on the stored URL
too (test engineer's find). 45 empty list elements in `Authorization`
ignored. 46 one real-HTTP test file. 47 the stub speaks TLS with a committed
test certificate. 48 test-only dependencies from the lock. 49 `http` to
loopback allowed (review: no `cfg(test)` swaps). 50 bodies print their
length in `Debug`. 51 `Retry-After` capped at `u32::MAX`. 52 one flaky
test's clock fixed.

**Push targets (53–72).** 53 `SaleKind` is the event name, no second enum.
54 a resource's own forms target itself (macros). 55 both `GET`s answer
`internal`. 56 re-registering replaces the whole subscription, keeps id and
`created_at`. 57 the form is strict about kind-specific fields. 58
`store_id` read from the app in the insert. 59 what a target's body shows.
60 the Query page hides `endpoint`, `auth`, the private key. 61 how the
VAPID key is kept and loaded. 62 no private-host check at registration (at
send, as webhooks). 63 `crate::push` beside the `push` crate. 64
`push-targets` merged the crate's fix (test engineer). 65 `http` to loopback
at registration. 66 a JSON array as a form is left to serde (follow-up for
all routes, 71). 67 the store cascade isn't tested by deleting a store. 68
P2 approved. 69 `store_id`/`failures` stay on the row. 70 `db::push` takes
the API's `ClientId`. 72 the stack takes the merge.

**Push deliveries (73–96).** 73 the `Topic` is a hash of the order id, not
the id (the id opens the checkout). 74 the Live Activity state: never the
order id, `currency` beside `amount`, `paymentURI` only while paying. 75
`waitingCount` from `app_orders.last_sale_event`. 76 every app's targets
are pushed; the order API's `sale.waiting` skips the maker. 77 `429`
doesn't count as a failure. 78 a retry that would fall past the TTL gives
up at once. 79 `sub` follows `public_uri` live. 80 `queue_order_event`
returns `Queued { webhooks, pushes }`. 81 links written as the web address
reads them. 82 one `give_up_push`. 83 an unreadable push is given up
alone. 84 no per-store share of a batch. 85 a push superseded in flight
keeps that attempt. 86 the payload's size isn't checked at queueing. 87 a
"Pushes to apps" card. 88 the Query page shows every column. 89
re-registration drops unsent pushes the target no longer takes (test
engineer's find). 90 P3 approved. 91 numbering 73–89 holds. 92 the engine
logs `order.pending` only on a change back, so one `sale.waiting`. 93
`waitingCount` means "waiting for a payment". 94 the design document
follows, in the Bell's PR. 95 `PushClient`'s own `Outbound` and the
webhooks' `Clock` stand. 96 the principal opens P3's PR.

**The push relay (100–130).** 100 two-part keys. 101 `logging.*` alone is
standalone-only. 102 `server.trusted_proxies` and
`server.upstream_timeout_ms` (its default is 4 s since the push audit,
decision 159). 103 `server.public_uri` is `Option`, refused
at start. 104 the relay's addresses follow `push::Endpoint`'s rule. 105
`Relay::start(&RelayConfig)`. 106 settings that need each other. 107 the
order of checks on a push; counted only once signed. 108 a handle lives on
only when APNs or FCM took a push. 109 `upstream-refused` 502 and
`internal` 500. 110 how APNs's and FCM's answers map. 111 HTTP/2 prior
knowledge always. 112 the 4096 bytes are APNs's and FCM's too. 113 the
relay never ends a Live Activity (replaced at review). 114 push-to-start's
JSON shape (push-to-start since taken out, 157). 115 one FCM token
exchange at a time (one budget with the send since, 159). 116 handles on one
connection, `STRICT`. 117 device tokens checked per platform, never
logged. 118 reqwest's `http2` reaches every client. 119 committed test
keys. 120 the `relay` feature and `relay.enabled`. 121 live-settings for
two embedded services. 122 CI runs. 123 APNs tokens lowercased (test
engineer). 124 P4 not approved at round 1 (four changes). 125 a delivered
or gone push is answered so whatever the database then does. 126 two
re-exports dropped. 127 the TOML form of `handles.apps` tested. 128 the
numbers stay (97–99 unused). 129 P4 approved at round 2. 130 a push the
handle's limit refuses still counts against its sender.

**Integration (131–138, the principal).** 131 one rebased line of four
stacked PRs, merged without squash. 132 the whole chain tested once, in
monokulo. 133 the paid alert's title carries the currency. 134 the Bell
pins the stack's head. 135 the design document updated in the Bell's PR.
136 `crypto-bigint` 0.7.5 stays until 2026-11-09. 137 the gates run on the
final stack. 138 this report's place.

**The Bell (B1–B35).** B1 git pins by revision (widened to six crates). B2
lock entries dated. B3/B30 `crypto-bigint` stays yanked-but-locked until
2026-11-09. B4 `GET /v2/locations/{id}`. B5 four fields read. B6 `rust.yml`
tests it; no release job yet. B7 `br_` ids. B8 `Bell::start(&BellConfig)`.
B9 `bell.public_uri` is the base address and the signature's URL. B10
`api_url` `https` or loopback. B11 the application id must match the
environment. B12 files under `live_settings::paths`. B13 public addresses
only unless `bell.allow_private_endpoints`. B14 registrations limited per
address; `trusted_proxies` loopback by default. B15 one registration per
endpoint. B16 32 per location. B17 a ding is sent once. B18 `200` once the
signature checks, whatever the body says. B19 `sub` is `public_uri`. B20
one timeout. B21 dings in flight finish at shutdown. B22 `verify_slice`,
constant time. B23 `tracing-subscriber` logging. B24 the token wiped on
drop. B25 problem types point at `docs/bell.md`. B26 the stand-in Square
grows a location and webhooks. B27 design §3 updated. B28 not wired into
the app. B29 `localhost` refused at registration (test engineer). B31 a
corrupt row fails the location's dings. B32 §5's pin wording. B33 the pin
moves to the stack's head. B34 the design document's §5/§6/§8/§10/§11. B35
the Bell's report.

## 6. Where the build left the design document

monokular's `docs/design.md` was written before the code and the build
departed from it in these places; the Bell's PR (#14) updates the document
so it says what was built (decisions 94, 135, B34).

| Design said | Built | Why (decision) |
| --- | --- | --- |
| `web-push-native`, `jwt-simple`, `gcp_auth`, `governor` | Written on `p256` 0.14 and the RustCrypto crates in the lock; `push::jwt`; a thin RS256 client on `rsa`; the workspace's `rate-limit` | 3, 4, 5, 6 |
| VAPID public key handed out at connect | Read from `GET /api/v1/push-targets`, live | 8 |
| Push targets by `PUT … per client_id`; no `/api/v1` resource yet | `GET`/`POST /api/v1/push-targets`, `GET`/`DELETE /api/v1/push-targets/{id}`, idempotent `client_id`, 8 per app | 7, 12 |
| `push_targets(id, app_connection_id, …)` | `push_targets(id, store_id, app_id, client_id, kind, endpoint, p256dh, auth, events, include_payment_uri, …)`, plus `vapid_keys` | 11, 13, 58 |
| `events` filters by name; "Monokular subscribes to `sale.*`" | A list of the seven names, all by default; no globs | 10 |
| `push.enabled`, `push.delivery.*`, `push.default_ttl` | `push.delivery_timeout_ms`, `push.max_attempts`, `push.default_ttl_secs`, `push.keep_days`, `push.target_failure_limit`; no `push.enabled` | 18 |
| `Topic: sale-1042` | The first 32 hex digits of the order id's SHA-256 | 73 |
| Live Activity state `phase, sale, amount, xmr, expiresAt, waitingCount, paymentURI` | Plus `currency`; `paymentURI` only while the sale takes a payment; never the order id | 74 |
| `sale.paid` → "phase paid, alert, then idle"; `apns-liveactivity` `event: update / end` | Every Live Activity push is an `update`; the relay never ends the activity; the app idles it | 113 |
| `sale.partial` → "amount left" | Phase `waiting`; the engine's event doesn't carry what arrived | 74 |
| `waitingCount`: "+1 more when sales overlap" | The store's app orders still waiting for a payment, this one included | 75, 93 |
| Relay routes `/v1/register`, `/v1/unregister`, `/v1/push/{handle}` | No unregister: handles expire (90 days) and die with the token | 20, 108 |
| `HandleStore` trait with an in-memory store; APNs/FCM faked behind traits | One `HandleStore` on `sqlite`; providers are settings pointed at stand-ins over real HTTP in `over_http` tests | 21 |
| Relay settings `relay.listen`, `relay.public_uri`, `relay.apps`, `relay.handle_ttl_days`, `relay.store_path`; on the admin page when embedded | Two-part keys (`server.listen`, `server.public_uri`, `handles.apps`, `handles.ttl_days`, `store.path`, …) under `[relay.*]`, read at start; `relay.enabled` alone on the admin page, read-only | 16, 100, 120 |
| Relay on `0.0.0.0:8443` | `0.0.0.0:8444` (8443 is the engine's) | 17 |
| `handles(… last_used_at)` | `… last_used_at, expires_at` | 108 |
| Relay answers `404`/`410`, `429` | problem+json: `201`, `404`, `401`, `413`, `429`/`503` with `Retry-After`, `502`, `500` | 109, 110 |
| The Bell checks the token with `GET /v2/merchants/me` | `GET /v2/locations/{id}`: proves token and location in one call | 24 |
| Bell settings: five keys | Plus `api_url`, `registration_failure_limit`, `registrations_per_minute`, `trusted_proxies`, `allow_private_endpoints`, `timeout_ms`; 32 registrations per location | B9–B16, B20 |
| `push` pinned "to a tag" | Pinned to a commit | B32 |
| §8 tests: "a test relay on the test's own router" | The relay is served on `127.0.0.1:0` and reached over HTTP in `relay_tests` | 132 |

## 7. Tests and gates

### Coverage

| Where | Tests | What they cover |
| --- | --- | --- |
| `crates/push` | 93 (89 in the crate, 4 `over_http`) | RFC 8291 Appendix A byte for byte; RFC 8292's example token; every parser's refusals (endpoints, keys, topics, headers, JWTs); `Sender`'s answer mapping for 201, 200, 404, 410, 413, 429, 5xx with `Retry-After`, 400, 401, a 307 not followed; 16 sends sharing one token; the request error leaving the endpoint out. The stub push service speaks TLS with a committed test certificate. |
| `crates/push-relay` | 59 (26 unit, 13 in-process, 18 `over_http`, 4 of the binary) | Settings (every key two parts, rules that tie them, the TOML form of `handles.apps`), handles (idempotent registration, lowercase APNs tokens, expiry only on delivery, the sweep), every refusal (forged VAPID, bad headers, oversize, unknown app or platform, limits with `Retry-After`, the sender's budget), APNs and FCM translation and their answers (gone, too large, unavailable with `Retry-After`, refused), the FCM token exchange shared and renewed, the served relay reached with `push::Sender`, the log holding no token, endpoint or body at any level, the binary's `--init`, `--help`, invalid settings and SIGTERM. |
| `crates/monokulo` | 80 (21 API scenarios, 6 on the key, 29 pipeline of which 11 `over_http`, 3 chain, 9 policy/payload/delivery units, 12 in `db`, the binary's key check) | `sk_`/`pk_` refused; another app's target 404; idempotent re-registration; the 9th target 409; kind-specific fields refused; the key surviving a restart and a wrong encryption key stopping the process; cascade on disconnect; racing registrations; queueing per target in the event's transaction; `sale.waiting` to the other apps and once on a retried order; superseding; TTL expiry without a request; give-up; pruning; the paid sale arriving `aes128gcm`, `Urgency: high`, a 32-character topic and a verifying VAPID header, decrypting to the Sale JSON; `429` with `Retry-After`; `410` deleting the target; 20 failures; `413`; private addresses refused without a request; timeouts; 16 in flight and 50 a batch; one token per origin; the whole chain through a served relay to stand-in APNs (alert and Live Activity) and FCM. |
| `crates/monokular-bell` | 71 (16 in-process, 25 `over_http`, 4 of the binary, 26 unit) | Square's documented signature example (recomputed); registration proving the location, refused tokens, another merchant's location, a redirecting or slow Square, the token left nowhere after a failed check; a completed payment dinging every till at its location and nothing else; other events dinging nothing; a webhook leaving no trace of its payment in the database files or a trace-level log; gone and failing subscriptions deleted; `429` not counted; redirects not followed; one VAPID token per origin; private endpoints refused; 32 per location, exactly, under racing registrations; fresh encryption per ding; the binary's `--init`, missing secrets and SIGTERM. |

The pipeline tests, the chain tests and the Bell's suite each run in well
under a second; the full monokulo nextest run stays within the owner's
targets (below).

### Gates, run on the final stack (`push-relay` at its head) on 2026-10-27

| Gate (as `.github/workflows/lint.yml` and `platform.yml` run it) | Result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | passed |
| `cargo clippy -p engine --all-targets --locked --no-default-features -- -D warnings` | passed |
| `cargo clippy -p engine-pow --lib --locked`, `-p engine-daemon --lib --locked` | passed |
| `cargo clippy -p push-relay --lib --bins --locked -- -D warnings` | passed |
| `cargo clippy -p monokulo --lib --bins --locked --no-default-features --features embedded-engine,tor -- -D warnings` | passed |
| `cargo +stable clippy -p push --all-targets --locked` and `--lib --locked -- -D warnings` (as the Bell builds it) | passed |
| `cargo metadata --locked --manifest-path fuzz/Cargo.toml` (the fuzz lock is current) | passed |
| The `test-support` grep (the feature only adds code) | passed |
| `cargo nextest run --workspace --locked --profile ci --features monokulo/snp --exclude xtask` | 2358 passed, 0 failed, 27 skipped (live nodes, scale, stagenet), 26.5 s |
| `cargo nextest run -p xtask --locked` | 112 passed, 2.0 s |
| `cargo test --workspace --locked --exclude xtask --doc --features monokulo/snp` | passed |
| `cargo xtask api openapi --out target/openapi.json` (the document with every recorded example) | passed |
| `cargo xtask test-summary --fail-slow` over the junit report (no test over its time limit) | passed |
| `cargo xtask ui --check` (the generated UI files are current; the stack adds a settings card) | passed |
| Browser tests (`e2e/browser`, `playwright test -c coverage-browser.config.js`) | 90 passed, 1.3 min (the fixture suites and the real-binary suites, Chromium) |

The suite's time is within the owner's targets (the whole local run well
under a minute excluding compile; no CI segment near five minutes).
Every engineer ran the same gates on their package before its review, and
the principal again at each review (decisions 68, 90, 124, 129).

monokular (`rust.yml`, on stable): `cargo fmt -p monokular-bell --check`, `cargo test --locked -p orf-codec
-p orf-codec-ffi -p monokular-bell`, `cargo nextest run -p monokular-bell`
(71 tests) and `cargo clippy --locked --workspace --all-targets -- -D
warnings`, on stable, against the pinned monokulo commit: all passed.

## 8. Risks

- **`crypto-bigint` 0.7.5 is yanked** (RUSTSEC-2026-0387, constant-time
  issues) and both locks hold it, under the two-week rule (decisions 136,
  B30). The fixed 0.7.9/0.7.10 are from 2026-10-25/26. Exposure: VAPID
  signing once per origin every 11 h and one ECDH per push, none timed from
  outside. Move both repositories on 2026-11-09, or now if the fix should
  win over the rule.
- **Real APNs, FCM and Square have not been reached.** Every provider test
  runs against a stand-in built from the public documentation (APNs's
  headers and reasons, FCM HTTP v1's message and OAuth, Square's signature
  with its documented example). The first run against the real services may
  find a detail the documentation leaves out; the sandbox endpoints are
  settings for that reason.
- **Two decisions change the wire for the apps:** the Live Activity state
  carries `currency` and never the order id (74), and the relay never ends
  an activity (113). The iOS `ContentState` must decode `currency` and the
  app must idle its own activity. Nothing is coded on the app side yet, so
  nothing breaks today.
- **reqwest's `http2` feature now reaches every reqwest client in a
  monokulo built with the relay** (118): webhook deliveries, rate providers
  and OTLP may negotiate HTTP/2 by ALPN with servers offering it. Standard
  behaviour, but new on those paths.
- **Two tables and a column, in production.** Additive only; the first
  start writes one `vapid_keys` row. The rollback is to not run the new
  binary; the tables are harmless to an old one.
- **The relay's tokens live in its own file in the clear.** As the design
  has it (a device token lets one push to a device only through the
  operator's own keys). The file should be treated like monokulo's
  database.

## 9. What is left

Follow-ups, none of them needed for the server side to be merged:

1. **The apps.** Android: UnifiedPush and FCM registration against the
   relay (`POST /v1/register`), `POST /api/v1/push-targets` with the
   endpoint, the receiver decrypting with the subscription's keys;
   Settings › Wake-ups against the Bell's `/v1/info` and `/v1/register`.
   iOS: the Notification Service Extension and the Live Activity's
   `ContentState` (with `currency`); no push-to-start opt-in, since the
   relay has none (decision 157). The contracts
   are the apps guide "6. Get pushes", the relay's page and `docs/bell.md`.
2. **Deployments**: the official relay and Bell (hosts are placeholders;
   Apple and Google keys; a release job for `monokular-bell`).
3. **Admin-page editing of `[relay.*]`** (16): needs the engine's
   settings-API plumbing for a second embedded service.
4. **Publishing `push` to crates.io**; until then re-pin the Bell to `main`
   after the merge (22, 134).
5. **The Square spike** (design §10.1): Other tenders, refunds, limits;
   the Bell's shapes were tested against stand-ins only.
6. **Smaller things recorded as left:** a form sent as a JSON array is
   taken by every `#[api::input]` route (66, 71: `extract::json_body`);
   the Connected apps card doesn't list an app's push targets; the `--init`
   header from live-settings mentions an admin page the Bell and the relay
   don't have; `cargo xtask pages docs` wasn't run for the new docs pages
   (it needs artifacts fetched in CI); the Bell's `localhost` rule (B29)
   could be mirrored in monokulo's `outbound::check_host`, where today a
   `localhost` endpoint is refused by the resolver at connect time (one
   counted attempt) rather than before it.
