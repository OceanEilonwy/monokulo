# Decisions: push, the push relay and the Monokular Bell

Made on 2026-10-27 without the owner to ask, while building Monokulo's
push system (the `push` crate, push targets and deliveries, the push
relay) and the Monokular Bell. Each says what was chosen, why, and what
else was possible, so any of them can be reversed knowingly. The Bell's
own entries are also in monokular's `docs/decisions/bell.md`.

Design: monokular `docs/design.md` §3 (wake-ups, the Bell), §5 (push),
§8 (monokulo code layout), §10 (delivery plan), §11 (open questions).

## Plan (the project planner)

Decisions 1 to 28 were made before any code, reading monokulo `main` at
8c7d902e6 and monokular `main` at abacc01. The principal Rust engineer may
overrule any of them; a replacement is recorded under the same number.

1. **Server side only today; no app wiring.** Two Rust engineers and one
   afternoon cover `push`, monokulo's targets and deliveries, the relay and
   the Bell. The Android work (UnifiedPush/FCM registration, Bell
   registration, Settings › Wake-ups) is Kotlin, needs a device or
   emulator to verify, and needs a deployed Bell and relay to mean
   anything. The iOS work hasn't started. Both go in the report as the
   next step with the exact contract they code against (the
   `/api/v1/push-targets` resources and the Bell's API).
2. **Not deployed, not published.** `bell.monokulo.com` and
   `relay.monokulo.com` stay placeholders; nobody here has the hosts or
   Apple/Google keys. `push` isn't published to crates.io: the Bell pins a
   commit (decision 22). The admin page doesn't edit the embedded relay's
   settings (decision 16).
3. **RFC 8291 and VAPID are written on `p256` 0.14, not taken from
   `web-push-native`.** `web-push-native` 0.5.0 (released 2026-07-26, old
   enough) pins `p256` 0.13 and `aes-gcm` 0.10, a RustCrypto generation
   behind the one already in `Cargo.lock` (`aes-gcm` 0.11.1, `hkdf` 0.13,
   `sha2` 0.11, `hmac` 0.13, `elliptic-curve` 0.14, `ecdsa` 0.17 via
   `p384`); taking it would duplicate those crates and put the version
   dance in our tree for the sake of about 150 lines of well-specified
   code. RFC 8291 Appendix A gives a complete known-answer vector (keys,
   salt, plaintext, ciphertext) and RFC 8292 §2.4 a worked JWT: the
   implementation is tested against both, and round-tripped through its
   own `decrypt`. The one new crate is `p256` 0.14.0 (2026-07-03).
4. **No `jwt-simple`.** A VAPID or APNs token is one ES256 signature over
   two base64url JSON segments. `jwt-simple` 0.15 brings `k256`, `p384`,
   `ed25519-compact`, `superboring` and several `hmac-sha*` crates for it.
   `push::jwt::Es256Key` does the signing and verification and is shared
   by VAPID and the relay's APNs client.
5. **No `gcp_auth`.** FCM HTTP v1 needs one RS256-signed JWT exchanged at
   `token_uri` for a bearer token. `gcp_auth` would add `ring` (a second
   C crypto library beside rustls's `aws-lc-rs`) and discovers credentials
   by itself from the environment, gcloud's files and the metadata server,
   which the settings rule forbids: secrets come from one declared
   environment variable. `rsa` 0.9.10 is already in the tree (pure Rust,
   `rsa::sha2` re-exports the digest it needs, `pem` feature reads the
   PKCS#8 key).
6. **No `governor`.** The workspace's `rate-limit` crate (fixed window per
   key, `check_with_retry` gives the `Retry-After`) is what the engine and
   monokulo already use; the relay and the Bell key it by VAPID public key,
   handle and address. One limiter in the workspace, not two.
7. **Push targets are `POST`-registered with an idempotent `client_id`,
   on a collection you can `GET`**, instead of the design's `PUT`.
   `api-macros` has `get`, `post`, `delete`; the API layout says every link
   names a resource you can `GET` and changes go to that same URL. So:
   `GET/POST /api/v1/push-targets`, `GET/DELETE /api/v1/push-targets/{id}`.
   A repeat `POST` with the same `client_id` replaces the subscription on
   the same target (the token-rotation case) and answers `201` with it,
   as `create-order` answers an idempotent retry. Adding `PUT` to the
   macros for one resource was the alternative; more surface than value.
8. **The VAPID public key is read from the collection, not handed out at
   connect.** The connect response stays `{app_key, api, onion_api}`;
   facts are fetched live (decision 50/54 of the app) so a rotated key is
   never stale. `GET /api/v1/push-targets` carries `vapid_public_key`.
9. **Only an app's key (`ak_`) may register push targets.** A shop server
   takes webhooks; a browser embed has no device. `who = app` on every
   push-target handler; the entry shows the link and form to `ak_` only.
10. **`events` is a list of the seven `sale.*` names (an enum), every one
    by default; no glob syntax.** "Parse, don't validate": a glob would be
    a string the server interprets at every event. Stored as a JSON array.
11. **Two target kinds, made unrepresentable to mix:** Rust
    `TargetKind::Encrypted { p256dh, auth } | LiveActivity { include_payment_uri }`,
    SQL `CHECK ((kind = 'encrypted') = (p256dh IS NOT NULL AND auth IS NOT NULL))`.
    `include_payment_uri` is set at registration: that is the "Show code in
    island" switch, so a device that turned it off never has the address
    pass through the relay.
12. **At most 8 push targets per app** (`too-many-push-targets`, 409): a
    till has one or two subscriptions per sender; an unbounded table per
    key is a way to fill the database.
13. **One VAPID key per instance, in `vapid_keys`, the private scalar
    encrypted with `MONOKULO_ENCRYPTION_KEY`** under its own
    `Binding::VapidKey`; made at the first start that finds no row.
    Subscription keys (`p256dh`, `auth`) are stored in the clear, as the
    design's table has them: they let one encrypt *to* a device and are
    useless without the sender's VAPID private key; the webhook secrets
    are encrypted because they let one *impersonate* monokulo. The `sub`
    claim is `server.public_uri` when set, absent otherwise (RFC 8292
    "SHOULD").
14. **`push_deliveries` keeps the plaintext payload and encrypts at each
    attempt.** RFC 8291 wants a fresh salt and ephemeral key per message;
    the payload is what `app_orders` already holds (amount, address,
    state), never items. Supersede rule: a newer unsent delivery with the
    same target and topic marks the older one `superseded_at_utc`, kept
    for `push.keep_days` so the deliveries can be inspected. A due
    delivery past `expires_at_utc` is given up as `expired before it could
    be sent`, never retried.
15. **Backoff 5 s, 20 s, 80 s, 320 s, at most `push.max_attempts` (5),
    never past the TTL.** Webhooks wait a minute first because a shop's
    server being down for a minute is normal; a push a minute late is
    worth little, and the TTL already bounds it. Dead targets: `404`/`410`
    deletes at once; `push.target_failure_limit` (20) consecutive failures
    deletes too, the design's "run of failures".
16. **The embedded relay's settings live in `[relay.*]` of monokulo's
    options file and `--relay-…`, read at start, not on the admin page.**
    Exactly how the embedded engine takes `[engine.*]`/`--engine-…`
    (`with_nested_settings`, `OptionsFile::scoped`), declared once in
    `push-relay` and registered by both binaries. Showing them on the
    admin page needs the engine's settings-API plumbing; a follow-up.
    `relay.enabled` itself is monokulo's own setting.
17. **The relay listens on `0.0.0.0:8444` by default**, not the design's
    `8443`, which is the engine's port. The relay is meant to be reached by
    apps and senders, so a public bind is the intent.
18. **No `push.enabled`.** Nothing runs until an app registers a target;
    a switch that leaves the form offered but refused would be a worse
    API. The worker idles on `Notify` and a 5 s poll like webhooks.
19. **Push endpoints must be `https`, never `.onion`, and pass the
    webhooks' private-address check** (`webhooks.allow_private_urls`
    governs both: same meaning, same operator). An app key could otherwise
    make monokulo `POST` to internal addresses. The checking resolver and
    host check move from `webhooks::delivery` to `crate::outbound`, used
    by both; `WebhookClient` keeps its API.
20. **No relay unregister route.** The design's `/v1/unregister` would be
    unauthenticated (the app holds no secret the relay knows) or
    sender-only, and no sender would call it (a dead target is just Web
    Push to monokulo). Handles die by `handle_ttl_days` since last use and
    on APNs/FCM saying the token is gone; the app re-registers on a token
    change. Nothing is written that nothing calls.
21. **Relay and Bell have no provider traits and no in-memory stores for
    tests.** A `HandleStore` trait with an in-memory implementation and
    `Apns`/`Fcm` traits with fakes would exist for tests alone. Both
    stores run on `sqlite::Pool` over a temp file (the `Inline` pool under
    `test-support`); APNs and FCM clients take their endpoint from a
    setting (`apns.endpoint`, `fcm.endpoint`, the sandbox being a real
    reason), so one `over_http` test per provider points them at a local
    HTTP/2 stub (plain `http` scheme uses prior knowledge).
22. **The Bell pins `push`, `live-settings`, `sqlite` and `rate-limit` as
    git dependencies on the monokulo repository at the head of the
    `push-crate` branch** (one `rev` for all four), so its settings follow
    the owner's model (TOML + CLI, env-only secrets, invalid values stop
    the process) instead of a second config scheme, and it stores and
    limits the way monokulo does. Those crates build on stable (checked
    2026-10-27: `cargo +stable check -p live-settings -p sqlite -p rate-limit`);
    `push` does too (`cargo +stable test -p push`). After the user merges the
    monokulo PRs the pin moves to `main`'s commit with `cargo update -p push`.
23. **Every new dependency is locked at a version released on or before
    2026-10-13.** In particular `tokio` is held at 1.53.x (1.54.1 is from
    2026-10-21) and `rusqlite` at 0.40.2 (0.41.0 is from 2026-10-24); the
    engineer checks each new `Cargo.lock` entry against crates.io and
    downgrades with `cargo update -p <crate> --precise <version>`.
24. **Bell registration proves the location with one call,
    `GET /v2/locations/{location_id}`**, whose answer carries the
    location's `merchant_id`: it checks both that the token is the
    merchant's and that the location is theirs, where
    `GET /v2/merchants/me` alone would prove only the first. The token is
    used for that one call and dropped.
25. **The Bell reads `payment.created` and `payment.updated` (status
    `COMPLETED`) and nothing else of the body**: `type`, `merchant_id`,
    `data.object.payment.location_id`, `status`, into a struct with
    exactly those fields (`#[serde(deny_unknown_fields)]` is not used, the
    rest is ignored, not read). Bodies are never logged, not even at debug.
26. **Monokulo's push tests go over real local HTTP only where a push
    must actually land** (a stub UnifiedPush endpoint on `127.0.0.1:0`,
    as the webhook tests host receivers), and those tests say `over_http`
    in their names. Queueing, superseding, TTL, give-up and target
    deletion are tested in-process against the database and the worker's
    outcome handling. The `push` crate's own tests are pure.
27. **Release artifacts:** `monokulo-push-relay` is built into the Docker
    image beside `monokulo-engine` (Dockerfile) and left out of the
    `monokulo-{version}-{target}.zip`, as the engine is. The Bell binary
    is built and tested by monokular's `rust.yml`; a release job for it
    is a follow-up with the deployment.
28. **The ids:** `pt_` for push targets (monokulo), `ph_` for relay
    handles, `br_` for Bell registrations (design), each `prefix_` + 32
    random hex, the `credentials::auth` style, made locally in the relay
    and the Bell so neither pulls `credentials` (argon2, telemetry).

## The `push` crate (rust-engineer-1)

29. **`MAX_PLAINTEXT` is 3993 bytes, not the plan's 4078.** RFC 8291 §4:
    4096 bytes of body less the 86-byte header, the 1-byte delimiter and
    the 16-byte tag. 4078 would have let a sender encrypt a body that a
    push service is free to refuse with `413`. A test encrypts exactly
    3993 bytes into exactly 4096 and refuses 3994.
30. **`Sender::send` takes the `Endpoint`, not a `Subscription`.** A
    live-activity target has an endpoint (the relay) and no keys, and an
    encrypted push is encrypted by the caller afresh at each attempt
    (decision 14), so `send` only ever needs where to send. Taking a
    `Subscription` would have forced made-up keys for live activities.
31. **`Authorization` holds the token and the key; `PushRequest` is
    `{authorization, options, message}`.** The plan's separate
    `sender_key` field would have been a second copy of `k`, free to
    disagree with it; `authorization.key()` is the one source. The
    options are the same `Options` a sender passes, so a round trip
    compares equal.
32. **`vapid::verify` takes a parsed `&Authorization`, not a header
    string.** A relay parses the whole request once
    (`PushRequest::parse`) and verifies what it parsed;
    `Authorization::parse` is the one reader of the header's syntax
    (scheme and names in any case, parameters in any order, quoted or
    not, a repeated parameter refused, unknown ones ignored).
33. **The JWT algorithm isn't the token's to choose.** `jwt::Header` has
    only `typ` and `kid`; signing always writes `ES256`, and verifying
    refuses any other `alg` (`none` included) and any `crit` before the
    signature is looked at. `verify_es256` returns the claims typed
    (`DeserializeOwned`), not as `serde_json::Value`, so `Claims` is
    parsed once: an `aud` that isn't exactly an origin's serialization is
    refused as unreadable claims.
34. **An endpoint's capability never reaches a log by accident.**
    `Endpoint` has no `Display`, and its `Debug` prints only the origin;
    `as_str` is for storage and `host` for log lines. `AuthSecret`,
    `VapidKey`, `Es256Key`, `CompactJwt` and `Authorization` print
    nothing secret either.
35. **`Es256Key::from_pkcs8_pem` (the APNs `.p8` key) comes with the
    relay**, which is its only caller; this branch has nothing that would
    call it.
36. **`PushRequest::parse` takes `&[u8]`, not `bytes::Bytes`**: axum's
    `Bytes` derefs to it, and the crate needs no `bytes` dependency.
    `httpdate` 1.0.3 (already in the tree) reads a `Retry-After` given as
    a date.
37. **Keys are read with or without base64url padding and written
    without.** Browsers and RFC 8291 write none, some server libraries
    pad; both mean the same key. JWT segments must be unpadded (RFC 7515
    §2). A key must be the 65-byte uncompressed point: a compressed one
    (valid SEC 1, not Web Push) is refused.
38. **What `PushRequest::parse` refuses beyond the RFC's syntax:** a
    missing `TTL` (RFC 8030 §5.2 lets a push service refuse it), an empty
    body (every push here carries one), and a body that is neither
    `Content-Encoding: aes128gcm` nor `Content-Type: application/json`. A
    `TTL` above `u32::MAX` seconds is read as `u32::MAX`, not refused: it
    is a valid `delta-seconds`. An `aes128gcm` body is checked for the
    shape of one record (key id of 65 bytes, record size of at least 18,
    no second record) without being decrypted.
39. **The sender's token cache reuses a token for less than 11 hours and
    forgets stale ones whenever it signs**, so it holds at most the
    origins pushed to in the last 11 hours. A clock set back re-signs
    rather than reusing a token from its future.
40. **The plan's `request_parts()` is `wire::headers`, crate-private.**
    `Sender::send` builds its request from it and the round-trip tests
    check it against `PushRequest::parse`; the URL and body need no
    building. Nothing outside the crate needs it, so it isn't public.
41. **The push service's response body is not read.** `send` maps the
    status and `Retry-After` only; a body could be any size, and nothing
    in it changes what the sender does. `Retry-After` as a date already
    past means no wait; one that reads as neither seconds nor a date is
    ignored, leaving the caller's backoff.
42. **An encrypted push is sent with `Content-Type:
    application/octet-stream`** beside `Content-Encoding: aes128gcm`, and
    `Urgency` is always written (also when `normal`), so what is sent
    never depends on a push service's defaults.
43. **CI checks `push` on stable** (lint.yml: `cargo +stable clippy -p
    push`, with its tests and as the library alone). The Bell builds it on
    stable from a git pin, and every other build here is nightly, so a
    nightly-only feature would otherwise reach the Bell unnoticed.

## The `push` crate's tests (the test engineer)

44. **An endpoint's 2048-character bound is checked on the URL as
    stored, as well as on the text given.** The check was on the text
    alone, and `Url` writes a non-ASCII path character as a percent-escape
    of each byte: a 2025-byte endpoint whose path was 1000 `é` passed, and
    stored 6025 characters. The text
    is still checked first, so a long one isn't parsed only to be refused.
    Found by a test; fixed in `Endpoint::parse`.
45. **Empty list elements in `Authorization: vapid …` are ignored**
    (`t=…,, k=…`, a leading or trailing comma), which RFC 9110 §5.6.1.2
    says a recipient MUST do; they were refused. A parameter given twice
    is still refused.
46. **`Sender::send` has one real-HTTP test file,
    `crates/push/tests/send_over_http.rs`**, every test named
    `…_over_http`: a push is the one thing that has to land, so the
    headers, signature and body on the wire, the answers read from real
    responses (201, 200, 404, 410, 413, 429, 5xx with `Retry-After`, 400,
    401, a 307 not followed), 16 sends at once, and a request error that
    leaves the endpoint's path out are checked over a socket. The status
    mapping itself stays tested in-process too. The five tests take about
    0.1 s.
47. **The stub push service speaks TLS with a committed test
    certificate** (`crates/push/tests/fixtures/`: a CA, and a certificate
    and P-256 key for `push.test`, made with the openssl CLI, valid to
    2126; the CA's key was discarded). An endpoint is https only, and no
    test-only way round that belongs in the crate. The alternative,
    generating certificates in the test with `rcgen`, would add a lock
    entry and, by default, `ring`. The client trusts only that CA
    (`tls_certs_only`) and resolves `push.test` to the stub.
48. **Test-only dependencies are crates already in Cargo.lock, at their
    locked versions:** `tokio`, `tokio-rustls` (on `aws-lc-rs`, the
    provider reqwest's rustls already uses, so no second one), `hyper`,
    `hyper-util` and `http-body-util`, plus `push` itself with
    `test-support` for decryption. The lock gains no package; the stable
    clippy and test runs build them too.

## Review of the `push` crate (the principal Rust engineer)

49. **An endpoint may be plain `http` when its host is this machine**
    (`127.0.0.0/8`, `[::1]`, `localhost`); anywhere else it is `https`.
    The plan's delivery tests (decision 26) post to a stub push service on
    `127.0.0.1:0`, as the webhook tests do with a receiver, and the
    delivery worker's client is production code with the platform's
    roots: trusting a test certificate there would be a `cfg(test)` swap,
    which the test rules forbid, and the push crate's own TLS stub can't
    be reached from it. TLS on loopback protects nothing (nothing is on
    the wire), and the private-address check (decision 19) refuses a
    loopback endpoint by default whatever its scheme, so a sender is as
    safe as before. The relay's `public_uri` already has this rule. `Url`
    canonicalizes every spelling of an IPv4 address, so `127.1` and
    `0x7f000001` are seen as loopback; `::ffff:127.0.0.1` and
    `push.localhost` are not.
50. **`PlainJson` and `Encrypted` print only their length in `Debug`.**
    They derived it, so `?request` on a `PushRequest` in a relay's log line
    would have written the body out as bytes. A Live Activity state is a
    sale's amount and, when the device asked for it, its address; the
    rule is that no request body reaches a log.
51. **`Retry-After` is read as at most `u32::MAX` seconds, the longest
    TTL.** A header of `99999999999999999999999` read as
    `Duration::from_secs(u64::MAX)`, which panics when added to an
    `Instant` or `SystemTime`; a library shouldn't hand back a value its
    caller can't add to a time. The caller still caps the wait at the
    push's own expiry (decision 15).
52. **The "pushes at once share one token" test signs every push at one
    second.** It had the 16 threads sign at `NOW`, `NOW + 1` and `NOW + 2`
    in whatever order the scheduler ran them: when a `NOW + 2` thread
    signed first, a `NOW` thread saw a token from its future and re-signed,
    as decision 39 has it, and the test failed once in a few hundred runs.
    The rule under test is sharing, not the clock; the clock-set-back case
    has its own assertion.

## Push targets (rust-engineer-2)

53. **`SaleKind` is the API's sale event name; there is no second enum.**
    The plan's `SaleEventName` would have been `SaleKind` again with a
    serde impl. `SaleKind` reads and writes as its stream name
    (`sale.paid`), and its schema (named `SaleEventName`) lists
    `SaleKind::ALL`'s names, so the stream, the push target's `events` and
    the stored JSON can't disagree. An unknown name is refused by
    deserialization, with the list of names in `detail`.
54. **A resource's own forms are sent to the resource itself.** The
    macros refused a form on a path with parameters (its target was the
    route template, `{…}` and all), so `unregister-push-target` couldn't
    be the plan's `#[delete(app, form = …)]`. A body's own forms now
    target its own path with the parameters filled in; a form another body
    offers (`Form<H>`) still targets its route, and a `Form<H>` whose
    route has parameters fails to compile (`FormTarget::TEMPLATE`). The
    alternative, a `DELETE` without a form, would have left the target's
    removal undiscoverable from its body, against the API layout.
55. **Both `GET`s answer `internal`.** The plan gave the list no problems;
    both read the database, which can fail, and the OpenAPI document must
    list what a route really answers. The recording shows each `500`
    with the table renamed away (and a trigger for `DELETE`).
56. **Registering a `client_id` again replaces everything about the
    subscription**: endpoint, kind, keys, events and
    `include_payment_uri`, and forgets `failures` and `last_ok_at_utc`,
    which were the old subscription's. Only the id and `created_at` stay.
    A replacement never counts against the limit of 8.
57. **The form is strict about what belongs to which kind.** `keys` with
    `live-activity`, no `keys` with `encrypted`, and `include_payment_uri`
    with `encrypted` (even `false`) are each `invalid-request` with a
    reason, rather than ignored: a client sending them has misunderstood
    the kind. An empty `events` is refused too ("leave it out for every
    one, or unregister"): a target that wants nothing is a target that
    shouldn't exist. Duplicated events are kept once, in `SaleKind::ALL`'s
    order.
58. **A target's `store_id` is read from its app in the same statement**
    (`INSERT … SELECT store_id FROM app_connections WHERE id = ?`), never
    passed in, so a target can't sit under a store its app isn't on. An app
    disconnected between its key check and the insert inserts nothing and
    is answered `401` as a disconnected app's key is.
59. **What a target's body shows.** Its keys never (they were the app's to
    give, not to read back); its endpoint yes, to its own app only (the app
    gave it, and comparing it is how an app knows a registration is
    current); `include_payment_uri` only for a `live-activity` target.
    Log lines carry the endpoint's host, never the URL (decision 34).
60. **The Query page hides a target's `endpoint` and `auth`, and the VAPID
    private key.** An endpoint is a capability (whoever holds it can push
    to the device), `auth` is a secret by name; `p256dh`, the public key,
    stays visible.
61. **How the VAPID key is kept.** The private scalar is hex inside
    `crypto::encrypt` (whose API takes text; the hex is zeroized after),
    bound to `vapid_keys.private_key:1`. Loading checks the stored public
    key is the private key's (a row edited by hand would otherwise hand
    apps a key their pushes don't verify against). `store_vapid_key` keeps
    the first row stored (`ON CONFLICT DO NOTHING`) and the loader uses
    what it reads back, so two starts racing can't end up with two keys.
    `AppState::for_tests` has a fresh key of its own (`Push::for_tests`);
    the recording stores a fixed one and loads it as a start does, so its
    examples are the same each run.
62. **Registration doesn't check whether the endpoint's host is
    private.** As for webhooks, that is checked at every send (decision
    19): DNS can change in between, and a check here would pass a name
    that later points inside.
63. **`crate::push` shares its name with the `push` crate**, as the plan
    laid it out; code in monokulo writes the crate as `::push::` and
    `db.rs` re-exports its module as `self::push`.

## Push targets' tests (the test engineer)

64. **`push-targets` takes `origin/push-crate` by a merge, not a rebase.**
    The branch was built on the crate's first commit; the crate's later
    endpoint bound (decision 44) is what keeps a stored endpoint readable:
    without it an app could register 1000 `é` (2025 bytes, stored as 6025
    characters), which then failed `Endpoint::parse` on every read, so
    that app's whole push-target list answered `500` for good. A rebase
    would have meant force-pushing rust-engineer-2's branch, so it is a
    merge, and the push targets' decisions (first numbered 44 to 54, the
    numbers the crate's review took meanwhile) are 53 to 63. A rebase of
    the stack later drops the merge commit.
65. **Registration takes plain `http` to loopback, as `Endpoint` does**
    (decision 49): the form's doc and the apps guide say so, and that
    Monokulo pushes there only when its admin allows private addresses
    (the send-time check, decision 19). Refusing it at registration
    would leave the delivery worker's real-HTTP tests nothing to register.
66. **A form sent as a JSON array is left as serde takes it.** A derived
    `Deserialize` reads a struct from a sequence too, so
    `["unifiedpush", "encrypted", …]` registers like the object. Every
    field's check still applies, and the same holds for every
    `#[api::input]`; refusing it belongs to `extract::json_body` for all
    routes at once, not to this one. Recorded, not changed.
67. **The `store_id` cascade isn't tested by deleting a store.** Nothing
    in Monokulo deletes a store, and older tables reference
    `store_connections` without a cascade, so the delete fails. The
    cascade that runs is the app's, tested from the dashboard and from
    `DELETE /api/v1/app`.

## Push targets' review (the principal)

68. **P2 is approved as it stands** (review round 1, at `dc2e93881`
    plus this entry). The whole diff against `origin/push-crate` was read
    and the gates run again: `cargo fmt --check`, `cargo clippy
    --workspace --all-targets --all-features --locked -D warnings`,
    `cargo +stable clippy -p push`, the fuzz manifest, the test-support
    grep, `cargo nextest run -p monokulo -p engine-protocol -p push -p
    xtask` (931 passed) and `cargo xtask api openapi`. Decisions 53 to 67
    stand. Nothing below changes code; the one edit is a rewrapped doc
    comment on the entry.
69. **A `PushTarget` carries `store_id` and `failures`, which nothing
    reads yet.** They are the columns the delivery worker (P3, next in
    this stack) reads: `failures` against `push.target_failure_limit`,
    `store_id` to pick a store's targets. A row struct is the row, as
    `AppConnection` and `DeliveryRow` are, and registration already resets
    `failures` (decision 56), so they stay rather than being added back
    in a day.
70. **`db::push` takes the API's `ClientId` scalar**, the first `db`
    module to import from `api::runtime::scalars`. A client id has one
    definition, and a row's `client_id` is parsed where its `endpoint` and
    `events` are, so an unreadable row is one error (`500`) rather than a
    body that can't be built. One crate, no cycle; a second db module
    needing a scalar would be the point to move the scalars down.
71. **A form sent as a JSON array (decision 66) stays a follow-up for
    `extract::json_body`**, all routes at once, outside this stack: every
    field is still checked, so nothing wrong is stored; refusing it on
    one route alone would make the routes disagree.
72. **The stack takes the test engineer's merge (decision 64).** P3
    (`push-delivery`) was branched from `4c8665177` and must take
    `aee1085de` and `dc2e93881` before its review; its decisions follow
    72. The final rebase of the stack onto `main` drops the merge commit
    and keeps the numbering 53 to 72 as it is here.

## Push deliveries (rust-engineer-2)

P3, `push-delivery`, on `push-targets` at `7c56066f4` (which has the
merge of decision 72). If the relay's entries (P4) land first, these
take the numbers after them; their content stands.

73. **A push's `Topic` is the first 32 hex digits of the order id's
    SHA-256, not the id's own 32 hex digits.** The `Topic` header crosses
    the push service and any relay in the clear (it is how they
    collapse), and an order's id opens its checkout, which shows the
    address and the amount. A hash is as good for collapsing (one per
    order, the same each time) and tells them nothing. Engine order ids
    are `order_` + 32 hex, so the plan's rule would have put the id
    itself in every header.
74. **What a Live Activity's state holds.** Never the order's id (the
    payload is plain through the relay and APNs; decision 73's reason).
    `sale` is the app's own `merchant_order_id`, left out when it gave
    none. `currency` is added beside `amount`: "$24.50" can't be drawn
    from `"24.50"` alone, and the design's table names a display string
    no server can format for the device's locale. `xmr` is trimmed
    (`0.1419`), as the payment URI's `tx_amount` is. `paymentURI` is sent
    only to a target with `include_payment_uri` *and* only while the sale
    takes a payment (`waiting`, `partial`): a paid or expired sale has
    no code to show, so the address doesn't cross the relay again.
    `sale.partial` is phase `waiting` (the rest can still come; the
    engine's event doesn't carry what arrived, so "amount left" isn't
    available) and `sale.double_spend` is `expired`, as the design's
    table has them.
75. **`waitingCount` comes from a new column, `app_orders.last_sale_event`**
    (migration 0053, nullable, with a `CHECK` on the seven names; no
    existing row changes: an order from before has long expired). Each
    queued sale event, pushed or not, records itself there in the same
    transaction; the count is the store's app orders whose latest event is
    none, `sale.waiting` or `sale.partial`, and that haven't expired.
    Deriving it from `push_deliveries` was the alternative, but those rows
    exist only for targets that asked for the event, and are pruned.
76. **Every app's targets in the store are pushed, not only the targets of
    the app that made the order**, as every app's event stream hears every
    app order: the tills of one shop all want to know of a sale. The one
    exception is **the order API's `sale.waiting`, which isn't pushed to
    the app that made the order**: the design's "the till that made it
    already shows it". The engine's `order.pending` later (the order
    waiting again after a double spend) is news to that app too, and is
    pushed to every target.
77. **A `429` doesn't count towards `push.target_failure_limit`.** The plan
    counted it; but a push service limiting this sender says nothing about
    the device, and twenty of them in a busy hour would delete a working
    target. A `413` (the payload can't be sent, a bug) and a VAPID signing
    failure (this instance's key) give up on the push without counting
    either. `5xx`, other refusals, timeouts, refused private addresses and
    connection failures count.
78. **A retry that would fall at or after the push's expiry gives the push
    up at once**, instead of the plan's "capped at expires_at", which
    would schedule an attempt with no TTL left to send. And each attempt
    sends as `TTL` what is left of the push's own (`expires_at - now`), so
    a push service never holds a push past the end the policy gave it.
79. **The `sub` claim follows `public_uri` while running.** The worker
    rebuilds its two senders (one per reach) when the setting changes, so
    a save applies without a restart; their signed tokens go with them. No
    `sub` while `public_uri` is unset or isn't `https` (a VAPID contact is
    `mailto:` or `https:` only).
80. **`Db::queue_order_event` takes a `push::Queueing` and returns
    `Queued { webhooks, pushes }`.** The settings it needs (`default_ttl`,
    the links' path) are read as each event is queued, like the webhook
    bodies' addresses (`Addresses::push_queueing`); the counts let the
    subscriber wake each worker only when it queued for it.
    `run_subscriber` and `follow` take the `Push` (a new argument at their
    four callers: `main.rs`, `mock-woocommerce`, two tests' and an
    example's).
81. **An `encrypted` payload's links are written as an app's key reads
    them, on the web address's path**: what the stream gives an app on
    the web address. An app connected over the onion address gets the same
    relative path, which is right whenever monokulo is served at its host's
    root (the onion address is). Two payloads per event, one per network,
    weren't worth it for one link the guide says to follow from the order
    anyway.
82. **One `give_up_push(id, at, reason)` instead of the plan's
    `give_up_push` and `mark_push_expired`**: the three reasons
    (`expired before it could be sent`, `no attempts left`, `unreadable:
    …`) are one update with different words. The plan's
    `delete_push_target_dead` is `delete_dead_push_target`.
83. **A due push that can't be read is given up on alone.** A row whose
    `event_type`, `urgency` or `topic`, or whose target's endpoint or keys,
    don't parse comes back from `due_push_deliveries` as an
    `UnreadablePush` and is given up as `unreadable: …` (logged as an
    error); failing the whole batch would have stopped every push behind
    it for good, as the order-event subscriber skips an unreadable event.
84. **No per-store share of a batch**, unlike webhooks (4 per store): a
    push's TTL bounds every backlog to minutes, an app has at most 8
    targets, and pushes go soonest first, so one store can delay another
    by a batch at most.
85. **A push superseded while its attempt is in flight keeps that attempt**
    (delivered, or its error) but is never scheduled again: the newer one
    is what the device should get next.
86. **The payload's size isn't checked when it is queued.** The test of the
    largest sale an app can make (a 120-character `merchant_order_id` of
    control characters, which JSON writes as 6 bytes each, a 40-character
    device name of emoji, an integrated address, the longest amount)
    shows both payloads well within `MAX_PLAINTEXT` and `MAX_BODY`; one
    that somehow isn't is given up at its attempt with an error log, the
    plan's runtime safety net. `MerchantRef`'s 120 became
    `scalars::MAX_MERCHANT_REF_CHARS` for that test to name.
87. **The push settings have their own card, "Pushes to apps", on the
    Payments tab**, after Webhooks; `webhooks.allow_private_urls`'s
    description now says it governs pushes too (decision 19), and the
    outbound onion error says nothing is sent over Tor "from here" rather
    than naming webhooks.
88. **The Query page shows every `push_deliveries` column.** The payload
    is what `app_orders` already shows (amount, address, reference); the
    topic is a hash; the endpoint, the one capability, isn't in the table.
89. **Registering a target again drops the unsent pushes it no longer
    takes** (found by the test engineer). A push's payload is made for its
    target's kind when it is queued, so a target registered again under
    the same `client_id` as another kind would have been sent the old
    payload as it was: an `encrypted` one, the whole sale with the order's
    id and address, in the clear to a Live Activity's relay. The same for a
    payment URI to a target that turned `include_payment_uri` off
    (decision 11), and for an event it no longer wants. Those unsent pushes
    are given up on in the registration's transaction (`its target was
    registered again for other pushes`). A new endpoint or new keys alone
    keep them: the payload is kept plain and encrypted at each attempt, so
    a moved UnifiedPush subscription still gets the sale in flight.

## Push deliveries' review (the principal)

90. **P3 is approved as it stands** (review round 1, at `7757f6704` plus
    this entry). The whole diff against `origin/push-targets` was read
    and the gates run again: `cargo fmt --check`, `cargo clippy
    --workspace --all-targets --all-features --locked -D warnings`, `cargo
    nextest run --workspace --locked --features monokulo/snp --exclude
    xtask` (2126 passed, 13.5 s), `cargo nextest run -p xtask` (112
    passed), `cargo xtask api openapi`, the test-support grep and the fuzz
    manifest. Decisions 73 to 89 stand, the test engineer's 89 included:
    giving up a re-registered target's unsent pushes in the registration's
    transaction is the right layer, as the payload is shaped there.
    Nothing below changes code.
91. **The numbering holds: 73 to 89 are P3's; the relay (P4) numbers from
    92 on.** `origin/push-relay` has no numbered entries yet, so nothing
    clashes; the final rebase of the stack onto `main` keeps these numbers.
92. **The engine logs `order.pending` only on a change of status back to
    pending** (`scanner::recompute_and_notify_in_tx`, never at an order's
    making), so the order API's `sale.waiting` (decision 76) is the only
    one an order's making queues; it isn't doubled by the log. Checked,
    because a second `sale.waiting` would have reached the app that made
    the order after all.
93. **`waitingCount` stays "orders still waiting for a payment"** (none
    seen yet, `sale.waiting`, `sale.partial`; decision 75), not every sale
    in progress: the field is named for the waiting phase, and a till
    learns of its store's seen and confirming sales from its own stream.
    The design's "+1 more when sales overlap" is read as overlapping
    waits.
94. **The monokular design document follows these decisions, as a
    follow-up in that repository**: §5's Live Activity fields gain
    `currency` (decision 74), its `Topic` example becomes a hash (73),
    and §6's `waitingCount` gets 93's meaning. The Monokular app's
    `ContentState` must decode `currency`. P5 (the Bell) touches §3 only;
    the planner carries the §5/§6 edit into the Bell PR or a docs PR.
95. **Two smaller things stand rather than being reshaped in review.**
    `PushClient` builds its own `Outbound` (two `reqwest` clients beside
    the webhooks' two): one shared `Outbound` on `AppState` is worth it
    when a third sender appears, not for two. And the worker tells the
    time by `webhooks::delivery::Clock`; a `crate::clock` is the move
    when a third worker needs it.
96. **The principal opens the draft PR**, `push-delivery` onto
    `push-targets`, as for P2 (#246): the engineer's brief left it open,
    and the stack's PRs are opened at approval so each carries its review.
## The push relay (rust-engineer-1)

Numbered from 100: the push deliveries (P3) are built at the same time
and number theirs from 73, so the two branches don't collide; the
principal renumbers, or not, when the stack is merged.

100. **Every relay setting has a two-part key**: `server.listen`,
     `server.public_uri`, `server.trusted_proxies`,
     `server.upstream_timeout_ms`, `store.path`, `handles.ttl_days`,
     `handles.apps`, `limits.*`, `apns.*`, `fcm.*`, `logging.*`, where the
     plan had `listen`, `public_uri`, `store_path`, `handle_ttl_days` and
     `apps`. Inside monokulo the relay reads `[relay.*]` through
     `OptionsFile::scoped("relay")`, which takes only the tables under
     `[relay]`: a value directly in `[relay]` is monokulo's own
     (`relay.enabled`), as `engine.mode` is beside `[engine.payment]`. A
     one-part key would have been read by neither handle, or refused by
     monokulo's. A test holds every key to two parts.
101. **`server.listen` is not standalone-only; `logging.*` alone is.** The
     plan listed `listen` in `STANDALONE_ONLY` and had `relay::start` bind
     `listen` too; the design gives the embedded relay its own listener
     (its limits and logs apart from the store pages). So
     `STANDALONE_ONLY = [logging.level, logging.format]`: inside monokulo
     the relay logs through monokulo's logger, under the service name
     `push-relay`.
102. **Two settings beyond the plan.** `server.trusted_proxies` (empty by
     default): a relay is reached over https, so it normally sits behind
     a reverse proxy, and the per-address registration limit would
     otherwise count every app as the proxy. It uses the abuse crate's
     `client_address` (the last `X-Forwarded-For` address no trusted
     proxy wrote) and `ClientIdentity` (IPv6 by its /64), as monokulo
     counts visitors. `server.upstream_timeout_ms` (10 000, 100 to
     120 000): how long APNs, FCM and FCM's token endpoint get; a tunable
     is a setting, not a constant.
103. **`server.public_uri` is `Option<PublicUri>`, not `required`.** It
     is needed to start a relay, not to load its settings: monokulo checks
     `[relay.*]` at every start with the relay off, and must not stop for
     a missing address then. `Relay::start` refuses `None`
     (`server.public_uri must be set`), so both binaries stop on it.
     `PublicUri` is an origin alone (no path): pushes are signed for the
     origin and the routes are at the root, so a path would be a promise
     the relay can't keep.
104. **The relay's addresses follow `push::Endpoint`'s rule.**
     `server.public_uri`, `apns.endpoint`, `fcm.endpoint` and a service
     account's `token_uri` are `https`, or `http` only to this machine
     (where tests run their stand-ins), never an onion address, never
     with credentials in them (`settings::ServiceUrl` wraps an
     `Endpoint`). The provider tokens and device tokens never cross a
     network in the clear.
105. **`Relay::start(&RelayConfig)`, not `(config, env, store)`.** The
     environment is read once, by `settings::load`, which builds a
     live-settings `Registry` over the file's values, the command line and
     the environment: every problem (a bad value wherever it came from, a
     secret that doesn't parse, settings that need each other) stops the
     process with all of them named. The section holds the parsed APNs key
     and service account, not their text. The store is opened from
     `store.path` by `start`. Both binaries serve through
     `Relay::serve(listener, shutdown)`, with each connection's address.
106. **Settings that need each other.** The APNs key needs `apns.team_id`
     and `apns.key_id`, and either id without the key is refused too (a
     forgotten variable, not a choice); an app with `apns_topic` needs the
     APNs key, one with `fcm_project_id` the service account. An app's
     table refuses an unknown field (`deny_unknown_fields`), so a
     misspelt `apns_topic` stops the relay instead of leaving a platform
     silently unserved.
107. **What a push is checked against, in order:** the handle id's shape
     (anything else is `404`, as an expired one), the body's size (read no
     further than 4096 bytes), the RFC 8030 request (`PushRequest::parse`),
     the handle, the VAPID signature against the handle's key and this
     relay's origin, then the limits, sender first. A push is counted only
     once its signature verifies, so nobody can spend a sender's or a
     handle's budget with pushes the sender never signed.
108. **A handle's life is pushed back only when APNs or FCM took a push**
     (or the app registered again), never by a request as such: otherwise
     anyone holding an endpoint could keep a dead device's handle forever.
     Lookups treat an expired handle as unknown; an hourly supervised
     sweep deletes them, so their tokens don't linger in the file. A
     handle whose app or platform has since left `handles.apps` is treated
     as gone (deleted, `404`).
109. **Problems beyond the plan's: `upstream-refused` (502) and
     `internal` (500).** APNs or FCM refusing a push for the relay's own
     reason (`BadTopic`, `InvalidProviderToken` that persists,
     `SENDER_ID_MISMATCH`, …) is neither the device's death nor a busy
     service: it is logged as an error and answered 502, which
     `push::Sender` takes as a server failure, retried and given up like
     any. A database failure is `500`. Each problem's `type` is a section
     of a new docs page, `https://docs.monokulo.com/push-relay/`, which is
     also the relay API's contract for app makers.
110. **APNs and FCM answers.** APNs: `410` and `400 BadDeviceToken` are
     gone; `413` too large; `429`, `5xx` and `403
     Expired/InvalidProviderToken` (the token is forgotten and signed
     anew) are `503`; anything else `502`. FCM: `UNREGISTERED`,
     `NOT_FOUND` or `404` are gone; `401` (the access token is forgotten),
     `429` and `5xx` are `503`; `413` too large; anything else `502`.
     `503` carries the service's `Retry-After` (seconds or a date) or 30
     seconds, kept within 1 second and a day. An answer's body is read to
     16 KiB at most, for its reason.
111. **APNs is reached with HTTP/2 prior knowledge always**, not only for
     an `http` endpoint: over TLS reqwest then offers only `h2` by ALPN,
     which is all APNs speaks, and over plain `http` it speaks h2c. One
     client either way.
112. **The 4096 bytes are APNs's and FCM's too.** A Web Push body may be
     4096 bytes, but base64 in a JSON envelope it no longer fits APNs's
     payload or FCM's data: the relay measures what it would send and
     answers `413` before sending (about 3000 bytes of ciphertext fit).
113. **The relay never ends a Live Activity: every Live Activity push is
     `event: update`** (replaced by the principal at review; the
     engineer's version ended it when the sale was paid or expired and
     `waitingCount` was 0, and asked for this call). Design §6 has one
     activity per shift, started by the app, back to idle between sales
     (set by the app) and ended by iOS after 8 hours or by the app. An
     `end` from the relay would take the shift's activity off the island
     at the first finished sale, and the app can't start another from the
     background; `update` keeps every option open, since the app can
     always end its own activity locally. So `paid` and `expired` are
     updates carrying their alert ("Paid · {amount}", "No payment yet"),
     `stale-date` is `expiresAt` while the sale is open, and the relay
     reads `phase` (`waiting`, `seen`, `confirming`, `paid`, `expired`),
     `expiresAt` (Unix seconds) and `amount` only: `waitingCount` is the
     app's to show and the relay no longer reads it. If a sender ever
     needs to end an activity remotely, that is a new field in the state,
     decided then. P3's `LiveActivityState` writes those names and types;
     its `Phase` comment saying the relay ends the activity changes at
     its rebase.
114. **A push-to-start's JSON is `{"attributes-type", "attributes",
     "content-state"}`**, each handed to APNs as it is, with event
     `start` and an alert (iOS shows a started activity only with one).
     The plan's "the plain JSON as attributes and content state" left the
     attribute type's name nowhere; this way the sender (or the app, which
     knows its Swift type) says it.
115. **FCM's access token is had once at a time.** A `tokio` mutex is held
     across the exchange, so pushes arriving together wait for one
     exchange rather than each signing an assertion; the token is reused
     until five minutes before it expires, and forgotten when FCM refuses
     it. The assertion is RS256 (PKCS #1 v1.5, `rsa::sha2::Sha256`) with
     the account's `private_key_id` as `kid`.
116. **Handles on one SQLite connection, in a `STRICT` table.** Every push
     reads its handle and, once delivered, writes its expiry, so a pool of
     readers would buy nothing. `STRICT` makes SQLite refuse a value of
     the wrong type, which the relay's own columns never need to hold.
117. **Device tokens are checked per platform**: APNs tokens are hex
     (an even number of digits), FCM's letters, digits, `-`, `_` and
     `:`, at most 1024 characters, so a token goes into a URL path as it
     is. `DeviceToken`'s `Debug` prints nothing, and no log line names a
     token, an endpoint, a provider token or a push; the error of a
     failed request is logged `without_url` (the APNs URL holds the
     token). A test captures every line at every level through a
     register-and-push of each platform and finds none of them.
118. **reqwest's `http2` feature reaches every reqwest client in a
     monokulo built with the relay** (Cargo unifies features): webhook
     deliveries, rate providers and OTLP may now negotiate HTTP/2 by ALPN
     with servers that offer it. HTTP's meaning is the same and rustls's
     ALPN is standard, so this is accepted rather than worked around. The
     one new lock entry is `h2` 0.4.20 (2026-10-06).
119. **Test keys.** A throwaway P-256 key in `.p8` form and a 2048-bit RSA
     key are committed under `tests/fixtures`; the service account around
     the RSA key is built in code, because GitHub's push protection
     refused the branch with it as a JSON file. The stand-ins are hyper on
     loopback (HTTP/1.1 and h2c), with dev-dependencies already in the
     lock; the tests that reach them, and the one that serves the relay
     and pushes to it with `push::Sender`, are named `…_over_http`. The
     binary's own tests run it as a process (`--init`, `--help`, invalid
     settings, listening and stopping on SIGTERM).
120. **Monokulo embeds the relay behind the default feature `relay`.**
     `relay.enabled` (false) is monokulo's own setting, in a section of
     its own (`RelaySwitch`, read at start), shown in the admin page's
     Server tab with monokulo's server settings, and `editable: false`:
     turning it on needs the `[relay.*]` settings the admin page can't
     edit (decision 16), and on without `server.public_uri` it would stop
     the next start. `[relay.*]` is read and checked at every start, the
     relay on or off. The relay's database defaults to `push-relay.db`
     beside monokulo's, as the engine's `engine.db` does (`store.path`
     still wins). Without the feature, `relay.enabled = true` stops
     monokulo naming `monokulo-push-relay`, and `[relay.*]` is refused with
     the same hint, also when the engine is remote.
121. **live-settings grows three things for two embedded services in one
     file:** an `OptionsFile` handle leaves several tables and carries a
     hint for each; `render_init_nested` takes a list of `Nested`
     services, so `monokulo --init` writes `[engine.*]` then `[relay.*]`,
     naming every service's secrets at the top; and
     `with_nested_settings` lists a nested service's environment-only
     secrets with the process's own after the options, so
     `monokulo --help` names `MONOKULO_RELAY_APNS_KEY` and
     `MONOKULO_RELAY_FCM_CREDENTIALS`.
122. **CI** (`lint.yml`) gains two clippy runs: `push-relay`'s library and
     binary without the test-support features its tests turn on, and
     monokulo without the relay feature, whose refusing code no test
     build compiles (monokulo's dev-dependency on itself turns the default
     features back on, so it can't have a unit test either; it was run by
     hand: `relay.enabled = true` and a `[relay.server]` table each stop a
     `--no-default-features --features embedded-engine,tor` monokulo with
     the messages above). The Docker image builds `monokulo-push-relay` beside
     `monokulo-engine` (decision 27); its port, 8444, is published only by
     a setup that runs a relay. The standalone relay logs to stderr only:
     it has no admin page to read a log store.
123. **APNs tokens are kept in lowercase** (test engineer). An APNs token
     is the app's hex spelling of the token's bytes, in either case; kept
     as sent, the same device, app and sender registered in both
     spellings had two handles, against the rule that the same four are
     one handle. FCM tokens are case-sensitive and kept as sent. No stored
     handles exist yet, so nothing is migrated.

## The push relay's review (the principal)

124. **P4 is not approved as it stands** (review round 1, at `143f9cea4`).
     The whole diff against `origin/push-targets` was read (40 files) and
     the gates run again: `cargo fmt --all --check`, `cargo clippy
     --workspace --all-targets --all-features --locked -D warnings`,
     `-p push-relay --lib --bins`, `-p monokulo --lib --bins
     --no-default-features --features embedded-engine,tor`, `cargo
     +stable clippy -p push --all-targets`, and `cargo nextest run -p
     monokulo -p live-settings -p push-relay -p push -p xtask --features
     monokulo/snp` (1075 passed, 2 skipped). All green. What must change
     before it merges: the Live Activity's `end` (113, replaced above),
     the answer to a delivered push whose expiry write fails (125), two
     re-exports nothing names (126), and one missing test (127). Decisions
     100 to 112 and 114 to 123 stand as written, 118 (reqwest's `http2`
     reaching every client) included: the workspace's `rustls` feature
     already brings reqwest's platform verifier and aws-lc-rs everywhere,
     so the relay adds no new TLS stack, only ALPN `h2`.
125. **A push APNs or FCM took is answered `201` even when the handle's
     expiry can't be written**, and one they said is gone is answered
     `404` even when the handle can't be deleted: the database failure is
     logged as an error and the sender is told what became of the push.
     As written, a failed `touch` after a delivered push answered `500`,
     which `push::Sender` takes as a server failure and retries, so a
     database hiccup would deliver the same notification twice. The
     database's state is a cache of what APNs or FCM know; the answer to
     the sender is the truth about the push.
126. **`push_relay::ServiceAccount` and `push_relay::AppName` are not
     re-exported**: nothing outside the crate names them (monokulo and the
     tests reach the relay through `settings` and `Relay`). A `pub use`
     nothing uses is dead code, as the standing rule has it.
127. **The documented options-file form of `handles.apps` is tested
     end to end**: the docs page and `--init` give `[handles.apps.monokular]`
     with `apns_topic` and `fcm_project_id` as a TOML table, and the only
     test of that form was a negative one (a misspelt field refused). One
     test reads such a table through `OptionsFile` and `settings::load`
     into `Apps` with the right bundle and project.
128. **The decision numbers stay as they are.** P3's run from 73 and the
     relay's from 100; the gap is harmless, and renumbering would break
     the references the entries, commits and reviews make to each other.
     The merged log keeps both runs, in the order the stack merges.
