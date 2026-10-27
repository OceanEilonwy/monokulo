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
