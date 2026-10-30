# Decisions: the Monokular Bell

Made while building the Bell (`crates/monokular-bell`) on 2026-10-27, without
the owner to ask, as package 5 of the push plan whose decisions are logged in
monokulo's `docs/decisions/push.md` ("plan" below means that log's part C).
Each says what was chosen, why, and what else was possible, so any of them can
be reversed knowingly.

**Moved 2026-10-29.** This log came with the Bell from the monokular
repository into monokulo, where the Bell is now `crates/monokulo-bell`
(crate and binary `monokulo-bell`); its numbering continues (59 and on, under
"The move into monokulo"). Entries 1 to 58 are as they were written, so the
paths, names and repository they mention are monokular's at the time. The
Bell's API and how to run it are at <https://docs.monokulo.com/bell/>
(`xtask/src/pages/docs/bell.md`), its design note in [../bell.md](../bell.md).

## From the plan

1. **The Bell builds on monokulo's crates by git revision** (plan 22, widened).
   `push`, `live-settings`, `sqlite` and `rate-limit` as the plan said, and
   also `abuse` (the client's address behind a trusted proxy, IPv6 by its
   /64, decision 14) and `service-runtime` (the SIGTERM/Ctrl-C signal, its
   10 s grace and the clock), all at one commit: the head of monokulo's
   `push-crate` branch, `abeb0ddfb`. They are the only entries in the root
   `[workspace.dependencies]`, so the revision is written once; the Bell's
   other dependencies are declared in its own manifest, as the repo's other
   crates declare theirs. Once the push PRs are merged, `rev` moves to
   `main`'s commit (`cargo update -p push --precise <commit>`). Pinning
   `push-relay`'s head instead was possible: its `live-settings` changes are
   for services embedded in monokulo, which the Bell isn't. Writing the
   proxy-address and shutdown code again here was the alternative to the two
   extra pins; both crates build on stable and bring only `sysinfo` new.
2. **Every new `Cargo.lock` entry is dated on or before 2026-10-13** (plan 23).
   Checked against crates.io's release dates. `crypto-bigint` (0.7.10 → 0.7.5),
   `ctutils` (0.4.6 → 0.4.2), `der` (0.8.3 → 0.8.2), `elliptic-curve`
   (0.14.3 → 0.14.1) and `inout` (0.2.5 → 0.2.2) were held at the versions
   monokulo's lock has. `tokio` was already in this repository's lock at
   1.54.1 (2026-10-21, from `tools/square-sandbox`); it is held at 1.53.3, as
   the plan asks, which `square-sandbox`'s `1.53.1` requirement allows.
3. **`crypto-bigint` stays at 0.7.5 although it is yanked.** RUSTSEC-2026-0387
   (2026-10-23): several operations meant to be constant-time could branch on
   secret values; fixed in 0.7.6, but 0.7.6 to 0.7.8 were yanked as well, and
   the only releases left are 0.7.9 (2026-10-25) and 0.7.10 (2026-10-26),
   days old. `p256` does its arithmetic with it, so the Bell's VAPID signing
   and RFC 8291's ECDH run on it. Kept at 0.7.5 because that is what monokulo's
   lock (and so `push`'s own tests) has, because releases this fresh and this
   churned are what the two-week rule is for, and because the exposure here is
   small: the VAPID key signs once per push service every 11 hours (`push`
   reuses the token), and an ECDH key is new for each ding and protects only
   `{"wake":"square"}`. **To be decided with monokulo**: move both locks to
   0.7.10 or later together, at the latest when it is two weeks old
   (2026-11-09), or now if the fix should win over the rule.
4. **Registration proves the location with `GET /v2/locations/{location_id}`**
   (plan 24), with `Square-Version: 2026-09-16`, the till's. The answer's
   `merchant_id` is what the registration keeps, and its `location.id` must be
   the one asked for. `401`, `403` (no `MERCHANT_PROFILE_READ`) and `404` (not
   the merchant's) and any other `4xx` are `square-refused`; `429`, `5xx`, no
   answer and an answer that isn't a location are `square-unavailable`.
5. **A webhook is read for `type`, `merchant_id`,
   `data.object.payment.location_id` and `status` alone** (plan 25), in two
   steps: `type` first, so any other event is dropped with nothing else read,
   then the payment's three fields into structs that hold only them. A body
   that doesn't read is described by where it stopped (line, column, kind),
   never with `serde_json`'s message, which can quote the value it refused.
6. **Built and tested by `rust.yml`; no release job yet** (plan 27). The
   workflow's test step runs `cargo test --locked -p monokular-bell` beside the
   codec crates; a job that publishes the binary comes with a deployment.
7. **Registration ids are `br_` and 32 random hex digits, made here** (plan 28).

## Made while building

8. **`Bell::start(&BellConfig)` takes the loaded settings alone.** The plan's
   `start(config, env)`: the two secrets are settings declared from the
   environment only, read with the rest by `settings::load`, so there is no
   separate environment to pass. `bell.public_uri` and
   `bell.square.application_id` are options-file settings, which
   `live-settings` can't mark required (only an unstored one can be), and a
   section has to accept every setting at its default; so they are `Option`s
   in `BellConfig`, and `Bell::start` stops with "… must be set" without them,
   before anything is opened. The push relay does the same with its address.
9. **`bell.public_uri` is the Bell's base address; Square's webhook URL is it
   followed by `/v1/square/webhook`.** `https`, or `http` to this machine (a
   push endpoint's rule, `push::Endpoint`), never an onion address, no user
   name, query or fragment; a path is kept (a proxy serving the Bell under
   one), a trailing slash isn't, so `https://bell.example.org/` and
   `https://bell.example.org` give the same URL. The signature is checked
   against exactly that URL: a subscription in the Developer Console with a
   trailing slash or a query is refused, and the `401`'s detail names the URL
   expected. The plan's type was `HttpUrl`, which takes plain `http` anywhere.
10. **`bell.square.api_url` is `https` or loopback `http` too.** It is sent a
    merchant's access token, so plain `http` across a network would hand it
    to anyone on the path. Unset, the environment's own:
    `https://connect.squareup.com` or `https://connect.squareupsandbox.com`.
11. **An application ID must match `bell.square.environment`.** `sq0idp-…` is
    production, `sandbox-sq0idb-…` sandbox; a mismatch stops the Bell, as
    Square would never send it a webhook.
12. **Its files are where `live_settings::paths` puts them**:
    `~/.config/monokulo/bell.toml` and `~/.local/share/monokulo/bell.db`, not a
    `monokular` directory. `paths` names monokulo's directory alone; another
    would need a `live-settings` change in monokulo, outside the pinned
    commit. `--options` and `bell.store_path` name any other place. The
    `--init` file's header mentions an admin page, which the Bell doesn't
    have: that text is `live-settings`'.
13. **Dings go to public addresses only, unless
    `bell.allow_private_endpoints`** (off by default). Any app can register any
    endpoint, so without this a registration could make the Bell `POST` inside
    its own network. As monokulo's `crate::outbound` does for webhooks and
    pushes: a resolver that refuses a name with any private answer at connect
    time, no redirects, no proxy, and an IP-literal endpoint checked before the
    request, and at registration too (`400`). The address rule is a copy of
    monokulo's `credentials::webhook_sign::is_disallowed_address` (and
    `telemetry`'s `embedded_ipv4`), as `credentials` would bring its password
    hashing and `telemetry` along (plan 28's reason). The tests turn it on, as
    their push service is on loopback.
14. **Registrations are limited per address: `bell.registrations_per_minute`
    (30)**, IPv6 by its /64, on `rate-limit`, counted before the body is
    read, as each costs a request to Square. The Bell listens on loopback
    behind a proxy, so `bell.trusted_proxies` is `127.0.0.1, ::1` by default:
    a proxy on the same machine is trusted for `X-Forwarded-For`, and every
    till isn't counted as the proxy. Webhooks aren't limited: only Square
    can sign them.
15. **One registration per subscription endpoint.** Registered again (the app
    starting again, or the till moved to another location) it keeps its id,
    takes the new location and keys, and its failures start over; the answer
    is `201` either way. An endpoint is a device's capability, so two rows for
    one would only ding it twice.
16. **A location takes at most 32 registrations; one more is
    `409 too-many-registrations`**, a problem the plan didn't have. A shop has
    a till or a few; unbounded, one merchant's token could fill the database.
    A constant, as monokulo's eight push targets per app is.
17. **A ding is sent once.** No queue, no retry: with a 60-second TTL a retry
    is worth little and the till's 30-second timer is the backstop. Each of a
    location's tills is sent to at once (a task per registration). `404`/`410`
    delete the registration; every other failure (an error status, no answer,
    a private address) counts towards `bell.registration_failure_limit`; `429`
    doesn't, as that is the push service limiting the Bell, not the till's
    subscription failing; a ding taken starts the count again.
18. **A webhook is answered `200` once its signature checks, whatever it
    says**; a body that doesn't read is logged as dropped (decision 5's
    description) rather than refused, since refusing only has Square send it
    again. A bad signature is `401 invalid-signature` with a warning in the
    log. Square's signature has no timestamp, so a captured webhook can be
    replayed: it costs a ding.
19. **Every ding's VAPID `sub` is `bell.public_uri`** when it is `https`
    (RFC 8292 takes no `http` one), so a push service's operator can find
    whoever runs the Bell; the same rule as monokulo's.
20. **One timeout, `bell.timeout_ms` (10 s), for Square and the push
    services.** Both are a single request whose caller waits (a till
    registering) or nobody does (a ding); two settings would tune nothing
    useful.
21. **A stopping Bell lets dings in flight finish**, within the same 10 s
    grace as requests (`Bell::dings_sent`). A webhook already answered `200`
    would otherwise lose its dings to a restart. The tests wait on it too.
22. **The signature's tag is compared with `hmac`'s `verify_slice`**, which is
    constant-time on `subtle`; the plan named `subtle` itself. No direct
    dependency is needed for the same comparison.
23. **Logging is `tracing-subscriber`, `logging.level` alone**, as the plan
    said, rather than monokulo's `telemetry` (OpenTelemetry, a log database).
    Colour only at a terminal; no JSON format setting yet. No line carries a
    body, a token or an endpoint, only registration ids and endpoint hosts;
    the tests assert it on a captured log at trace level.
24. **The access token is wiped from memory when dropped** (`Zeroizing`), its
    `Debug` prints nothing of it, and it is dropped as soon as Square's
    request is sent. What is wiped is the string it arrived in; the copy in
    the request's `Authorization` header is reqwest's own and is freed, not
    wiped, with the request.
25. **Problem `type`s point at sections of `docs/bell.md` on GitHub**, the
    Bell's API page, written with this build; the Bell has no site of its own.
26. **The stand-in Square (`tools/fake-square`) answers
    `GET /v2/locations/{id}` and can send each sale to a Bell as a signed
    `payment.created`**, so `bell.square.api_url` has the local use the plan
    gave it and a Bell runs end to end on one machine (README).
27. **`docs/design.md` §3 says what was built**: the location check, the
    settings, the git pins and the failure rule. §5 still says the Bell pins
    `push` "to a tag" (it pins a revision, decision 1): outside §3, which is
    all this package was to touch.
28. **Not wired into the app** (plan 1). The Android app registering with a
    Bell (Settings › Wake-ups, `/v1/info`, `/v1/register` with its UnifiedPush
    or relay endpoint) is the next step; `docs/bell.md` is the contract.
29. **A `localhost` endpoint is refused at registration when private
    endpoints are off** (found by the edge tests). The registration check
    only looked at IP literals and left every name to the resolver, so
    `http://localhost:…` (which `push::Endpoint` takes, as loopback) was
    registered with `201` and then failed every ding until
    `bell.registration_failure_limit` deleted it. `localhost`, `localhost.`
    and every name under it are loopback whatever a resolver says (RFC 6761
    §6.3), so `outbound::check_host` now refuses them, at registration
    (`400`) and before each ding; other names are still the resolver's to
    check at connect time. monokulo's `crate::outbound` check, which this
    copies, should get the same rule.

## From the principal's review (2026-10-27)

30. **`crypto-bigint` stays at 0.7.5 until 0.7.10 is two weeks old**
    (decision 3, decided). The two-week rule stands: 0.7.6, 0.7.7 and 0.7.8
    were each yanked within days, which is the churn the rule is for, and the
    Bell's exposure is a signing every 11 hours per push service plus one
    ECDH per ding over `{"wake":"square"}`, none of it reachable to time from
    outside. Both locks (monokulo's and this one) move together to 0.7.10 or
    later on 2026-11-09, or at once if the owner says the fix wins over the
    rule or the advisory is upgraded. Logged in monokulo's `push.md` as well
    when its PRs are reviewed.
31. **A stored row that doesn't read fails the whole location's dings**
    (`RegistrationStore::at_location` returns `StoreError::Corrupt`), rather
    than skipping the row. Only the Bell writes rows, so a corrupt one means
    the file was edited or the schema drifted: failing closed and loud (an
    error log line per webhook) is better than dinging whatever does read
    next to it. Revisit if a migration ever changes the columns in place.
32. **`docs/design.md` §5 now says `push` is pinned to a commit**, not a tag
    (decision 27 had left the wrong wording for scope reasons; a stale
    statement about what was built is a doc bug, so fixed in review).

## From the integration (2026-10-27)

33. **The pin moves to the head of monokulo's `push-relay` branch,
    `35711592a`** (decision 1 refined; monokulo `push.md` 134). After
    `push-crate`, the push crate gained `Es256Key::from_pkcs8_pem` and
    live-settings the handling of several embedded services in one options
    file, and one `rev` serves all six crates, so the one commit that has
    everything is the stack's final one. The Bell builds, lints and tests
    against it on stable as before. After the monokulo stack is on `main`,
    `rev` becomes `main`'s commit (`cargo update -p push --precise`).
34. **`docs/design.md` says what was built, beyond §3** (monokulo `push.md`
    94 and 135): §5 (the VAPID key read from `GET /api/v1/push-targets`,
    `currency` in the Live Activity state, the `push` and `push-relay`
    layouts and the crates table as built, the relay API without an
    unregister route and with its problems, the hashed `Topic`, every Live
    Activity push an `update`, the two-part relay settings under `[relay.*]`
    and port 8444, the push targets' resources and `POST`, the events' TTL
    and topic, the delivery path's retries and no `push.enabled`), §6
    (`currency`, `waitingCount`'s meaning, the app idles the activity), §8
    (monokulo's layout, migrations, settings, tests without provider
    traits), §10 (what is built) and §11 (the pin, `crypto-bigint`). The
    design's sections stay; what changed is stated where it stands.
35. **The Bell's part of the project report is `docs/reports/bell.md`**,
    pointing at monokulo's `docs/reports/bell-and-relay.md`, which covers
    both repositories: what was built, how to try it, every decision, the
    design's deviations, tests and gates, risks and what is left.

## From the push audit (2026-10-27)

36. **A location named by an alias is the till's mistake, answered `400`
    `invalid-request`** (push audit #4). Square answers
    `GET /v2/locations/main` with the main location's own ID; the Bell
    answered that mismatch `503 square-unavailable`, so a till retried a
    registration that could never succeed. A `200` for another location is
    now `LocationError::NotById`, permanent, with a detail saying to give the
    location's own ID. The answered ID isn't stored instead: the till matches
    webhooks by the ID it chose, so a registration under another one would
    never ding it. `main` isn't refused by name before Square is asked, so
    any other alias Square has or adds is handled the same way.
37. **An endpoint already registered moves only for its own merchant, or
    with its own auth secret; otherwise `409 endpoint-registered`, without
    the id** (push audit #16). `ON CONFLICT (endpoint) DO UPDATE` let anyone
    with any Square account and a till's endpoint take its registration (new
    merchant, location and keys) and learn the id that unregisters it. The
    same merchant still moves and re-keys its tills freely (an app
    reinstalled, a till moved between locations): its own token is the
    proof. Another merchant needs the 16-byte auth secret the till
    registered with, which never leaves the till and the Bell, compared in
    constant time (`subtle`, already in the tree under `hmac`): that is the
    till signed in to another account without unregistering first, which
    would otherwise be stuck at `409` with dings its old merchant still
    sends. A challenge pushed to the endpoint was not chosen: it needs a
    second round trip and state for something the auth secret already
    proves.
38. **A payment rings once, and a refunded one never** (push audit #3).
    Square sends `payment.updated` of a payment that stays `COMPLETED` (the
    processing fee filled in, a refund, Monokular's own refund of an expired
    Monero sale), and each one dinged: harmless on Android, a false "New
    sale" alert on iPhone and iPad, which the Notification Service Extension
    can't withdraw. The Bell now reads the payment's `id` (a Square ID,
    nothing about the buyer) and whether `refunded_money` is present (never
    its value), ignores a refunded payment, and keeps the ids it rang in
    memory, for 10 minutes and 10,000 at most, oldest forgotten first.
    These are constants, not settings: they bound memory and cover the
    minute or so in which Square sends a payment's updates, and no operator
    has a reason to tune them. In memory only, so the Bell still writes
    nothing of a webhook to disk; a restart costs one extra ding at most,
    which design §3 accepts. `payment.updated` isn't dropped altogether:
    a card payment that is `APPROVED` first becomes `COMPLETED` by one.
    `PaymentId` has no `Display`, so no log line can name it.

39. **The Bell reads its client's address from every `X-Forwarded-For`
    line, through monokulo's `abuse::identity::client_address`** (push
    audit #8, the Bell's half). The Bell read only the header's first
    line, so behind a proxy that adds a line of its own rather than
    joining the client's, a client named its own address in a line
    before the proxy's and had a fresh registration minute each time.
    The helper now takes the `HeaderMap` and walks every line from the
    last, so no caller can read one line again; the monokulo pins move to
    `31c3cab9d` (push-relay, after monokulo's and the relay's halves), the
    first commit with it. The pin also brings the shared `outbound` crate
    (monokulo `push.md` 154) in through `push`; the Bell's own
    `outbound` module stays until push audit #2's Bell step replaces it.
    The README now says how a relay operator sizes
    `limits.per_sender_per_minute` for a whole Bell (push audit #15): the
    relay counts every merchant's tills as one sender.

40. **The Bell stops on one token made before anything waits for it, and
    the design has no push-to-start** (push audit #10 and #18, the Bell's
    half; monokulo `push.md` 156 and 157). The Bell called
    `service_runtime::shutdown::signal()` twice, once for `bell.serve`'s
    graceful shutdown and once for the wait before the grace period: two
    listeners for the same signal, the shape #10 names, where each hears
    only signals after it is made. The monokulo pins move to `a0ff005f0`
    (push-relay, with every group 4 fix), where `signal()` is private and
    `shutdown::on_signal()` hands back a `CancellationToken` whose
    handlers are installed before it returns: the Bell makes it where it
    made the first listener, gives a clone to `bell.serve` and waits on
    the token itself, so its start-up behaviour is unchanged. No test of
    its own: with `signal()` private the two-listener shape no longer
    compiles, and the binary's SIGTERM test covers the stop. The pin also
    takes the relay's other group 4 fixes (#11, #12), which the Bell
    doesn't call. Design sections 5 and 6 lose the `apns-pushtostart`
    platform, its row and the opt-in, matching the relay; push-to-start
    comes back with a monokulo target kind that writes it.

41. **Dings go out through monokulo's `outbound` crate, not a copy of it**
    (push audit #2; monokulo `push.md` 154). The Bell's `outbound` module
    was monokulo's rule copied line for line (decision 13), and had
    drifted: it refused `localhost` names and monokulo didn't, monokulo
    refused onion hosts and it left that to `push::Endpoint`. The rule now
    lives once, in the `outbound` crate (no password hashing, nothing that
    needs nightly), pinned at the same monokulo commit as the others
    (`a0ff005f0`, already the pin): the Bell builds its push client with
    `outbound::client_builder(reach)` plus its `User-Agent` and
    `bell.timeout_ms`, and calls `outbound::check_host` at registration
    and before each ding, so it now refuses an onion host itself as well.
    The module and its three tests are gone; the address table (both
    copies' lists together) and the resolver's tests are the crate's. A
    Bell test of its own resolver was not added: the client is the
    crate's, built in one place, and the Bell's tests over HTTP still
    check what it does with it (no redirect followed, private and
    `localhost` endpoints refused at registration and at each ding).

42. **Each merchant's tills get at most
    `bell.dings_per_merchant_per_minute` dings a minute, 120 by default,
    the rest dropped** (push audit #15, the Bell's half; monokulo
    `push.md` 144, 151). Every ding is signed with the Bell's one VAPID
    key, which a push relay counts as one sender, so one busy or hostile
    merchant (32 tills a location, as many locations as it likes) could
    spend the relay's `limits.per_sender_per_minute` for every merchant
    behind the Bell, whose tills then fell back to their timers. Only the
    Bell knows merchants apart, so the share is taken here, with the
    shared fixed-window `RateLimiter` keyed by `MerchantId`, in memory (a
    restart gives a fresh minute). Per merchant, not per location: a
    merchant opens locations at will, and the merchant is the account
    that pays. A ding is counted when it is about to be sent, after its
    host check, so a refused endpoint costs nothing; over the share it is
    dropped and logged, not counted as its till's failure, as a ding a
    push service limits (decision 17): a ding a minute late is worth
    nothing, so it is not put off as monokulo's pushes are. 120 is about
    fifteen payments a minute at a location with eight tills; the
    setting's description and the README say to keep it well below the
    relay's per-sender limit. The README's sizing note (decision 39)
    stays: many merchants at once can still reach the relay's limit,
    which its operator sizes.

## From the second build: points of sale (2026-10-28)

The owner's decision: the Bell keeps its name and becomes generic across
points of sale, Square ported unchanged as the first adapter and Clover the
second. The research is Clover's documentation as of today (the build's
shared notes, `clover-research.md`); where it was thin the assumption is
named in the decision.

43. **A point of sale is a variant of one enum, and each adapter a module
    of its own, not a trait object.** `PointOfSale { Square, Clover }`
    (`src/pos.rs`) is the wire name everywhere: a registration's
    `point_of_sale`, `/v1/info`'s keys, a ding's `wake`, the problem slugs,
    the webhook path and the settings table. `Adapters` (`src/lib.rs`)
    holds an `Option<Arc<…>>` per adapter. A trait was the alternative:
    `async fn` in a trait isn't object-safe without `async-trait` (a new
    dependency), the two adapters differ in kind (an HMAC over URL and body
    against a static header plus a one-off handshake), and a `match` makes
    the compiler list every place a third point of sale must touch. The id
    and token types, `ProofError` (was `LocationError`), `BodyError`, the
    API client and `request_error` moved from `square.rs` to `pos.rs`,
    their messages no longer naming Square; `square.rs`'s
    `read_notification` still answers its `Notification` (the route turns
    `Completed` into a `Ring`), so its unit tests keep their assertions,
    only building their ids with `try_from` now that the fields are
    `pos.rs`'s.

44. **One registration body for every point of sale:
    `{point_of_sale, access_token, location_id, subscription}`; the
    adapter says what its location is** (for Clover the merchant id). The
    answer is `{registration_id, point_of_sale, application_id}`. A clean
    break from `square_access_token` and `square_application_id`: no Bell
    is deployed, and the till that speaks to one (Wake-ups, PR #16) is
    unreleased and changed in the same work, so both spellings would be
    dead code at once. There is no shim, and the Bell's PR and the app's
    say they land together or the app's first. A `merchant_id` field for
    Clover alone was the alternative: two shapes for the till to build and
    the Bell to check.

45. **`/v1/info` lists `points_of_sale` by wire name, each with
    `application_id` and `environment`**, the points of sale that are off
    absent, Square first when both are (a hand-written `Serialize` in
    `PointOfSale::ALL`'s order, since a map would sort them). The old
    top-level `square_application_id` and `environment` go (44's reason).

46. **Problem slugs stay per point of sale, `{pos}-refused` and
    `{pos}-unavailable`, from one variant carrying the point of sale**
    (`Problem::Refused(pos)`, `Problem::Unavailable(pos)`), so Square's are
    unchanged and a till matches its own. `invalid-signature` is one slug
    from `InvalidSignature(pos)`, titled "Not signed by Square" as before
    and "Not from Clover" for Clover. One generic slug was the
    alternative, and would have changed Square's.

47. **Migration 0002 adds `point_of_sale` (default `square`) and leads the
    location index with it; an endpoint is owned by its
    `(point_of_sale, merchant_id)`, or by whoever holds its auth secret.**
    Rows from before are Square's, as the Bell served Square alone until
    then. `MAX_PER_LOCATION` counts within a point of sale. The plan had a
    stored value that isn't a point of sale read as `Corrupt` (decision
    31): every query names its point of sale, so such a row is never read
    and the check would have been dead; the column's `CHECK` bounds its
    length.

48. **A Clover merchant is its own location**: its row has
    `location_id = merchant_id`, and a Clover registration's `location_id`
    is the merchant id. Clover has nothing below a merchant (a business
    with several sites has a merchant id, and a token, for each); one
    `NOT NULL` column kept means one query and one index for both.

49. **Clover's webhook is authenticated by its static `X-Clover-Auth`
    header, the app's Auth Code, compared in constant time; the one-off
    verification handshake is answered `200` and its code logged at
    `info`.** Clover has no body signature and no timestamp, so the code
    is the only integrity there is: a leaked code lets anyone ding the
    tills, at one request to Clover each within the merchant's share, and
    a replay is absorbed by the ten-minute memory. The handshake comes
    without the header, so without it the Bell reads at most 1 KiB and
    takes only an object of exactly one key, `verificationCode`, whose
    value is a UUID (8-4-4-4-12 hex digits): anything else is `401`
    (never `413`) with nothing logged, so nobody writes free text into
    the Bell's log (the acceptance's D4). With the header, a handshake is
    logged the same way. The code is the one value of a body the Bell ever
    logs: Clover's handshake value, not merchant data, and the operator has
    no other way to see it. A batch whose `appId` isn't
    `bell.clover.application_id` is dropped with a warning naming nothing
    (defence in depth, not authentication).

50. **A Clover `P:` `CREATE` dings; `UPDATE`, `DELETE` and every other
    object never do.** The body carries no tender, amount or status, so an
    `UPDATE` would be a refund, tip or void days later and, past the
    ten-minute memory, a false ding to every till; a card payment's
    `CREATE` dings too, as the Bell can't tell it from a Monero one, and
    `bell.dings_per_merchant_per_minute` bounds that. This is the main
    difference from Square, whose webhook says the payment completed. An
    object's id is read only when it is a payment's; a `P:` id that isn't
    one makes the batch unreadable (taken and dropped, as Square's).

51. **A point of sale is on iff its `application_id` is set; its secret
    must be set iff it is on; at least one is on; its webhook route exists
    only when it is on.** `required: true` on a secret can't say "when the
    section is on", so the rules are `FieldError`s in
    `BellConfig::from_snapshot`, every problem named at once, and
    `StartError::NoPointOfSale` in `Bell::start` (the settings library
    builds the all-defaults section as a fallback, which must load). A
    secret set for a point of sale that is off stops the Bell: it is how a
    misnamed section would otherwise pass unnoticed (the acceptance's
    D3). A webhook to a point of sale that is off is axum's `404`, its
    body never read, and a registration for one is `400` naming the ones
    this Bell runs, its token never used.

52. **The rung-payment memory and each merchant's ding share are keyed by
    `(point_of_sale, id)`**: two points of sale may give out the same id
    text. A Clover webhook for merchant `M` never dings a Square
    registration of `M`, and doesn't spend its share.

53. **A ding says which point of sale woke the till: `{"wake":"square"}`
    or `{"wake":"clover"}`**, from `PointOfSale::wake`; the headers are
    unchanged.

54. **`tools/fake-clover` is written with the Bell, by one owner, and
    cherry-picked onto the app branch unchanged.** The Bell's one-machine
    run and the till's both need it; two owners on two branches would
    diverge. Standard library only, Clover's documented shapes, and QA's
    switches (expiring and revoking tokens, a refresh-token cap of three,
    refusing tender writes). Its port is 8740, not the acceptance's 8739:
    `tools/square-sandbox` listens on 8739 and the README runs it beside
    fake-square. It knows two merchants, each with a never-expiring test
    API token (as Clover's dashboard issues one) printed at start, so a
    Bell's registration can be tried with curl alone, and the "another
    merchant's token" case has a merchant to be.

55. **Clover's registration proof is `GET /v3/merchants/{merchant_id}`
    with the token, whose answer's `id` must be the one asked** (research
    assumption A6: a token is one merchant's; to be confirmed in a
    sandbox). Statuses map as decision 4's: any `4xx` but `429` refused,
    `429`, `5xx`, no answer or an answer that isn't a merchant
    unavailable. Only `id` is read (the owner's name and email beside it
    land in no type); redirects are off; the Bell's `User-Agent` is sent,
    as Clover requires one. A `200` for another id is `invalid-request`
    ("location_id must be the merchant ID Clover answers the token for"),
    as Square's alias is: asking again gives the same answer. The
    acceptance had it `clover-refused`; the plan's API, which the till
    builds on, has `400`, and the till treats both as its own mistake.

56. **`[bell.clover].environment` picks one region (`sandbox`,
    `north_america`, `europe`, `latin_america`; default `north_america`),
    one Bell serving one.** Each is a developer dashboard and an API host
    of its own (`apisandbox.dev.clover.com`, `api.clover.com`,
    `api.eu.clover.com`, `api.la.clover.com`); the web hosts the till signs
    in on are the till's business. A Clover App ID carries no environment
    prefix, so the Bell can't check it against the environment as it does
    Square's (decision 11); `bell.clover.application_id` takes 1 to 64
    letters, digits, `-` and `_`, Square's id rule (Clover's are 13
    letters and digits). `bell.clover.api_url` serves a stand-in, as
    Square's does.

57. **The crate's version is 0.2.0** (its API changed), and
    `docs/bell.md`, `docs/design.md` §1, §3 and §9, `docs/reports/bell.md`
    and the README say what is built.

58. **A security fix wins over the two-week rule; `crypto-bigint` moves to
    0.7.10 now** (the owner, 2026-10-29; replaces decisions 3 and 30).
    New dependencies and upgrades still wait until a release is two weeks
    old, except when a version in use is yanked or has a security
    advisory: then it moves at once to the fixed release, after its
    changelog is read, and what the fix pulls in is held at the oldest
    version that release accepts. `crypto-bigint` goes 0.7.5 (yanked,
    RUSTSEC-2026-0387, constant-time leaks under `p256`'s VAPID signing
    and each ding's key agreement) to 0.7.10, and `ctutils` 0.4.2 to
    0.4.6, the oldest 0.7.10 accepts. monokulo's lock moves in the same
    step (its decision 160).

## The move into monokulo (2026-10-29)

Made by the packaging team (principal, two Rust engineers, an OpenWrt
engineer, QA) on the owner's plan: the Bell moves into monokulo with its
history, named `monokulo-bell`, as one PR merged by rebase. The owner's plan
is final; these are the calls it left open.

59. **The Bell's history moves, replayed linearly onto monokulo's `main`.**
    Its five commits in monokular (`49f8544`, `f274ad7` #14, `13d020a` #19,
    `54ac16f` #17, `cc72561` #21; monokular squash-merges, so none is a
    merge) are rebuilt with only the Bell's paths in them, each on the one
    before, the first on monokulo's `main` (`a5845fce4`): the result of
    `git filter-repo` followed by `git rebase --root --onto origin/main`.
    The plan named `git filter-repo` (v2.47.0, downloaded as its single
    script, sha256 `67447413…7d94`); running downloaded code was refused in
    the session that did the move, so the same filter was done with git's
    own plumbing (a temporary index per commit, `read-tree --prefix` for
    each kept path under its new name, `commit-tree` with the original
    author, email and date), and every moved path's tree checked identical
    to monokular's at `cc72561`. A merge of the unrelated history was the
    alternative; monokulo takes rebase merges only, so a PR can't hold one.
60. **The moved paths are `crates/monokular-bell`, `tools/fake-clover`,
    `tools/fake-square`, `docs/bell.md` and `docs/decisions/bell.md`.** Both
    stand-ins move whole: they are what the Bell's dev loop and the till's
    end-to-end run talk to, and `49f8544`'s other 13 files (the app's) fall
    away, which is "the Bell-relevant parts of fake-square" at the history
    level; splitting the 155-line script would break the one-process loop.
    monokular's `docs/reports/bell.md` stays there (a report on that
    repository's work), and its `docs/design.md` isn't filtered (it is the
    app's design): its Bell part is copied by hand into this repository's
    `docs/bell.md`.
61. **Authors and dates are kept; each message is rewritten only to point
    its PR number at monokular (`(monokular #14)`) and to name its source
    commit.** The replayed commits aren't buildable states of monokulo's
    workspace (the Bell isn't a member until the commits after them): a
    bisect that lands on one should skip it.
62. **Paths are renamed as they are replayed** (`crates/monokular-bell` to
    `crates/monokulo-bell`, `docs/bell.md` to `xtask/src/pages/docs/bell.md`),
    so `git log --follow` works from the new paths.
63. **The Bell takes the workspace's version, 0.1.0, not its own 0.2.0.**
    One version for every crate here, and a release tag names it.
    `GET /v1/info`'s `version` says `0.1.0` from now on; the till only
    shows it. The API page's "second API" note loses its version number.
64. **The crate keeps edition 2024** (the others here are on 2021):
    changing it is churn with no gain, and nightly builds both.
65. **No `license` field.** monokulo's crates have none and the repository
    has no LICENSE file; monokular's was `GPL-3.0-only`. The owner owns
    both; flagged in `docs/reports/packaging.md` so a licence can be added
    to monokulo if wanted.
66. **The Bell logs through the `telemetry` crate**, as the push relay
    does: `logging.level` with telemetry's check, and a new
    `logging.format` (`auto`, `json`, `pretty`), so the two standalone
    services here write one log shape. `tracing-subscriber` stays a
    dev-dependency only, for the tests' captured log.
67. **Setting keys stay `bell.*` and the options file `bell.toml`**
    (already under `~/.config/monokulo/`); only the crate, the binary, the
    environment variables and the `User-Agent` (`monokulo-bell/<version>`)
    change name.
68. **The secrets are `MONOKULO_BELL_SQUARE_WEBHOOK_KEY`,
    `MONOKULO_BELL_CLOVER_AUTH_CODE` and `MONOKULO_BELL_VAPID_KEY`; the
    `MONOKULAR_BELL_*` names are still read for one release.** Through
    `live_settings::Env::with_renamed` (generic, as `with_retired` is for
    options): an unset new name is read under its old one, each old name
    set is one warning at start, and old and new set to different values
    stop the Bell, naming both and neither value, since which was meant
    can't be told. The Bell's `settings::RENAMED_ENV` lists them; the first
    version tag after the one that ships them removes it. Reading both
    silently, or the new name only, were the alternatives; the owner asked
    for one release's grace.
69. **lint.yml's stable-toolchain clippy of `push` and `outbound` goes, a
    production clippy of the Bell and a boundary check come.** The stable
    step existed for the Bell as a stable git consumer in another
    repository, which it no longer is (monokulo's `push.md` 161 retires
    its 43); it can come back the day another appears. `cargo clippy -p
    monokulo-bell --lib --bins` checks it without its tests'
    `test-support`. "Monokulo never depends on the Bell" fails the build
    when `cargo tree -p monokulo --all-features` (normal, build and dev
    edges; the engine and everything under it) holds `monokulo-bell`, after
    making sure the crate exists so it can't pass for the wrong reason
    (tried both ways: a dev-dependency on the Bell fails it).
70. **The Docker image builds `monokulo-bell` too**, beside
    `monokulo-push-relay`, for setups that run it as a container of its
    own.
71. **The problem types are `https://docs.monokulo.com/bell/#<slug>`**,
    sections of the API page on the docs site, as the relay's are
    `…/push-relay/#<slug>`; they were monokular's GitHub file. The till
    reads only what follows the `#`, so it is unchanged.
72. **How to run a Bell is a "Run one" part of the `/bell/` page** (the
    monokular README's "Running a Bell", brought to the new names), as the
    relay's page has its own; the install guides for a computer and a
    router that come with the packaging link it.
73. **The stand-ins stay stdlib Python under `tools/`, run by no CI job
    here**, as in monokular: the Bell's own tests have stand-ins of their
    own in Rust. Their docstrings name the new variables.
74. **The Bell has `--check` and a status listener on loopback
    (`status.listen`, `127.0.0.1:8447`); TLS stays its proxy's.** Recorded
    with the relay's in `docs/decisions/packaging.md` 1 to 12: `Bell::check`
    is the first half of `Bell::start`, so the adapters' log lines now come
    after every check passes rather than as each adapter is built; the
    status counts registrations, says which points of sale are on and how
    the last ding went, never a merchant, location, token or endpoint.

## Several environments at once (2026-10-30)

The owner's decision: one Bell serves several environments of each point
of sale at the same time (Square production beside its sandbox, Clover
North America beside its sandbox or another region), the Monokular Android
app changed in parallel against the same contract. No Bell with data is
deployed, so it is a clean break, as the API layout prefers; old settings
fail loudly rather than being read.

75. **The unit is an integration, a point of sale and one of its
    environments; everything is keyed by it.** `Integration { Square(SquareEnvironment),
    Clover(CloverEnvironment) }` (`src/pos.rs`), six in a fixed order
    (Square production, sandbox; Clover north_america, europe,
    latin_america, sandbox), each with its settings table
    `[bell.<pos>.<environment>]`, its secret, its webhook route
    `POST /v1/<pos>/<environment>/webhook` (axum's `404`, unread, for one
    that is off, as 51 has it) and its adapter (`Adapters` is a list of
    `Adapter::Square` or `Adapter::Clover`, each knowing its environment).
    A registration's body gains a required `environment`
    (`{point_of_sale, environment, access_token, location_id,
    subscription}`, `deny_unknown_fields` kept) and its answer says it
    (`{registration_id, point_of_sale, environment, application_id}`); an
    environment its point of sale doesn't have is `invalid-request`
    ("environment must be one of Square's: production, sandbox."), one that
    is off `invalid-request` naming what this Bell runs ("this Bell runs
    Square production and sandbox, and Clover europe"), its token unused
    either way. The token is proved against that integration's API; the ding
    lookup, an endpoint's owner (its own merchant in its own integration, or
    whoever holds its auth secret), `MAX_PER_LOCATION`, each merchant's ding
    share and the rung-payment memory are all keyed by the integration, so
    sandbox and production ids never meet (they are made by different
    systems, and the same text in both is two merchants). The ding still
    says only `{"wake":"square"}` or `{"wake":"clover"}` (53): the till
    knows its own environment. This supersedes "One application per POS per
    Bell" (`docs/bell.md`), 11 (the table now names the environment, and an
    id whose prefix isn't its table's stops the Bell, naming the table it
    belongs in), 44's body and answer, 45's `/v1/info`, 47's and 52's keys
    (an integration where they say a point of sale) and 56 (one Clover
    region per Bell). Problem slugs stay per point of sale (46): a till
    knows which environment it asked.

76. **The settings of a Bell of one environment per point of sale stop it,
    naming what replaced each.** `settings::REPLACED` holds
    `bell.square.application_id`, `environment` and `api_url` and the same
    three of `bell.clover`, each with its replacement as "is now called"
    reads; the options file is read with
    `OptionsFile::with_renamed(REPLACED)` ("line 2: bell.square.application_id
    is now called bell.square.production.application_id or
    bell.square.sandbox.application_id, by the application's environment:
    rename it in the options file"), and the command line takes each as a
    hidden option only to refuse it with the same words. `REPLACED_ENV`
    holds `MONOKULO_BELL_SQUARE_WEBHOOK_KEY`,
    `MONOKULO_BELL_CLOVER_AUTH_CODE` and their `MONOKULAR_BELL_*` names: one
    set stops `settings::load`, naming the new variables and never the
    value. Reading an old key as production's was the alternative: the
    environment it meant can't be told for Square's api_url or Clover's
    code, and a guess serves the wrong one. `OptionsFile::with_hint` (one
    message for any unknown key under `bell.square`) was the other; the
    rename table names each key, and needed no change to `live-settings`.
    `RENAMED_ENV` keeps the VAPID key's old name alone (68 had all three).

77. **`/v1/info` and the status nest by point of sale and environment**:
    `{"points_of_sale": {"square": {"production": {"application_id": …},
    "sandbox": {…}}, "clover": {"europe": {…}}}, "vapid_public_key", "version"}`,
    the integrations that are on alone, a point of sale with none left out;
    the status says every one, `"square": {"production": "ready", "sandbox":
    "off"}, "clover": {…four…}`. One hand-written `Serialize`,
    `pos::ByIntegration`, gives both in `Integration::ALL`'s order whatever
    order they come in (a map would sort them). `--check` says a line per
    integration that is on (its application and webhook URL) and one for
    the rest ("off: Square sandbox, and Clover north_america, latin_america
    and sandbox"); the start logs "serving Square production" and so on.

78. **Migration 0003 adds `environment TEXT NOT NULL` (default
    `production`, its length bounded as `point_of_sale`'s), sets Clover's
    rows to `north_america`, and leads the location index with
    `(point_of_sale, environment)`.** Rows from before are in the
    environment their point of sale defaulted to then, the one a Bell
    without `environment` set ran; a Bell that ran Square's sandbox or
    another Clover region needs its tills to register again, said in the
    migration and `docs/bell.md`. `ADD COLUMN` with a default and an
    `UPDATE` follow 0002; rebuilding the table to drop the default was the
    alternative, and every insert names the environment anyway.

79. **The six integrations' settings come from two small macros and a
    table, not six blocks.** `IntegrationSettings<Id>` holds one's
    `application_id`, `api_url` and secret as `live_settings::Setting`s
    built in a `const` by `integration!` (through `square!` and
    `clover!`, which add each point of sale's words), every key, variable
    and description made with `concat!` from the environment's name. The
    rest stay in one `settings!` block (in a private module, since the
    macro also declares `ALL`), and `settings::ALL` is that block's
    followed by the integrations', joined in a `const fn`; the Bell's
    section is its own keys and the integrations'. A test holds every
    setting to one section, once, and each integration's three to its
    table and its variable to its name.

80. **On OpenWrt each integration is a section named
    `<point of sale>_<environment>`, of its point of sale's type:
    `config square 'square_production'`, `config clover 'clover_sandbox'`.**
    The plan had the environment alone as the name (`config square
    'sandbox'`), but UCI names a section once in a config whatever its
    type, so Square's and Clover's sandboxes would be one section. The init
    script writes each into its table and refuses a section whose name
    isn't one of its point of sale's environments, naming it. The shipped
    config has `square_production`, `square_sandbox` and
    `clover_north_america` with their options commented out; the LuCI page
    adds the others with each card's Add. `uci-defaults` upgrades an
    install from before: `config square 'square'` or `config clover
    'clover'` with an `application_id` is renamed for its environment
    (Square's by its id's prefix, Clover's by its `environment` option,
    `north_america` unless set) and loses that option, an empty one is
    deleted, and the old secret file takes the integration's name; run
    again it does nothing, and a section or file of the new name is never
    overwritten (tried in the OpenWrt rootfs image, twice over; the rootfs
    test upgrades an old Clover section). The LuCI page has a card per
    integration with its state, webhook URL, application, API address and
    secret. Most operators use production alone, so Square production and
    Clover North America are shown, and the other Clover regions and the
    two sandboxes are each behind a `<details>` ("More Clover regions",
    "Sandbox (for developers)") that starts closed unless one in it has an
    application ID or the status says it is on, so nothing set is ever
    hidden. LuCI's `form` has no disclosure, so the cards are rendered as
    usual, marked, and moved into the `<details>` after the map renders
    (and again when a Save redraws it), in LuCI's own look.

81. **The upgrade gives way only to an empty section, and an integration's
    `api_url` is refused while its application isn't set** (after review).
    A new section of the old one's name with no `application_id` (the
    shipped example, an Add pressed and left) is replaced by the old
    section, which has the application; one with an `application_id` is
    the operator's own, so both are kept for them to sort out, and the old
    secret file stays under its old name, never paired with the hand-made
    section. An `api_url` in a table whose `application_id` isn't set
    stops the Bell, as a secret set for an integration that is off does
    (decision 51): it is most likely a table misnamed, which would
    otherwise pass unnoticed.
