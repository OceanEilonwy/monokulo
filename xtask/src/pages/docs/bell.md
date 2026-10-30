# The Monokular Bell

The Bell turns a point of sale's payment webhooks into dings for the tills
registered at that location: the tills of the
[Monokular](https://github.com/OceanEilonwy/monokular) app, which take
Monero at a Square or Clover counter. It serves Square, Clover,
or both, in as many of their environments at once as it is set up for: each
integration, a point of sale's environment, is an adapter, on when the
Bell's settings name an application for it (`[bell.square.production]`,
`[bell.square.sandbox]`, `[bell.clover.europe]`, …). A ding is a Web
Push whose only content is an encrypted `{"wake":"square"}` or
`{"wake":"clover"}`: it wakes the till to ask its point of sale for new
payments itself, so a lost, late, repeated or forged one costs one request
to the point of sale at most.

This page is the Bell's API, as a till (or anyone writing one) uses it, and
what each of its errors means, then [how to run one](#run-one). The choices
behind it are in its
[decision log](https://github.com/OceanEilonwy/monokulo/blob/main/docs/decisions/bell.md);
its code is `crates/monokulo-bell`, the `monokulo-bell` binary. An iPhone
till, or an Android one with only Google's services, takes its dings through
[the push relay](../push-relay/).

A point of sale is named by its wire name, the one spelling everywhere:
`square` or `clover`, in a registration's `point_of_sale`, `/v1/info`'s
keys, a ding's `wake`, the webhook paths and the problems `<pos>-refused`
and `<pos>-unavailable`. Its environments are named the same way: Square's
`production` and `sandbox`, Clover's `north_america`, `europe`,
`latin_america` and `sandbox`, in a registration's `environment`,
`/v1/info`'s keys below the point of sale, the webhook path
`/v1/<pos>/<environment>/webhook` and the settings table
`[bell.<pos>.<environment>]`. Each integration is its own: its own
application, webhook URL and secret, and its merchants are never another
environment's, even with the same id.

This is the Bell's third API. The first, Square's alone, took
`square_access_token` and answered `square_application_id` (decision 44);
the second served one environment per point of sale, its webhooks at
`/v1/<pos>/webhook` and its registrations without an `environment`
(decision 75). Both are gone, with no shim: no Bell with data was deployed,
and the till that speaks to one changes with it.

## What the Bell sees and keeps

**Square** sends the Bell every webhook of each of its applications, for every
merchant signed in through it, with each payment's amount, card and buyer.
The Bell checks the signature, reads a payment event's `type`,
`merchant_id`, and its payment's `id`, `location_id`, `status` and whether
`refunded_money` is there (never what it says), and nothing else.

**Clover** sends the Bell every webhook of each of its apps, batched: ids and times
only, `{"appId", "merchants": {"<merchant>": [{"objectId", "type", "ts"}]}}`.
The Bell checks the `X-Clover-Auth` header, reads `appId`, each merchant
key, and each update's `objectId` (its id only when it is a payment's,
`P:…`) and `type`, and nothing else. The one value of a body it ever logs is
Clover's verification code, once, for the operator
([below](#post-v1-clover-environment-webhook)).

Neither body is stored, logged or put into an answer. The ids of the payments
it rang, with their integration, are kept in memory for 10 minutes (10,000
at most), so each payment rings once, and nowhere else. A merchant's access
token is used for one request to its point of sale at registration and
dropped.

Its own SQLite file (`bell.store_path`) holds one table: per registration,
its point of sale and environment, the merchant and location, the till's push subscription
(endpoint and keys), when it was made, the last ding its push service took,
and how many in a row it didn't. A log line names a registration id and an
endpoint's host at most.

A Clover merchant has no location below it: its merchant id is its location
(decision 48), and a Clover registration's `location_id` is the merchant id.

**At a Clover merchant every payment dings.** Clover's webhook carries no
tender, amount or status, so the Bell can't tell a Monero payment from a
card payment there: each payment created wakes the merchant's tills, which
ask Clover and find nothing new when it was a card. A Square payment dings
only when it completes. `bell.dings_per_merchant_per_minute` bounds both.

## Routes

Request and answer bodies are JSON. Errors are `application/problem+json`
(RFC 9457), their `type` this page's section for the problem.

### `GET /v1/info`

What a till needs to choose this Bell:

```json
{ "points_of_sale": {
    "square": { "production": { "application_id": "sq0idp-…" },
                "sandbox": { "application_id": "sandbox-sq0idb-…" } },
    "clover": { "europe": { "application_id": "ABCDEFGHIJKLM" } } },
  "vapid_public_key": "BOS-CZ1F…", "version": "0.1.0" }
```

`points_of_sale` has a key for each point of sale this Bell serves, and
under it one for each of its environments the Bell serves, none for the
others (at least one integration is there). The order is fixed: Square,
then Clover; Square's `production`, then `sandbox`; Clover's
`north_america`, `europe`, `latin_america`, then `sandbox`. A till signs in
to its point of sale's environment through the `application_id` given
(each sends webhooks only to the application a merchant signed in
through), and makes its push subscription
for `vapid_public_key` (the `applicationServerKey`, or the
`vapid_public_key` it registers with monokulo's push relay): every ding is
signed with that key.

### `POST /v1/register`

One body for every point of sale:

```json
{ "point_of_sale": "square",
  "environment": "production",
  "access_token": "EAAA…",
  "location_id": "L8GWD5R9QB376",
  "subscription": { "endpoint": "https://…", "keys": { "p256dh": "…", "auth": "…" } } }
```

```json
{ "point_of_sale": "clover",
  "environment": "europe",
  "access_token": "…",
  "location_id": "CLVMERCHANT01",
  "subscription": { … } }
```

`point_of_sale` and `environment` are required, and together name an
integration this Bell serves: an environment its point of sale doesn't have
is [`invalid-request`](#invalid-request) naming the ones it has, and one this
Bell doesn't serve is too, naming what this Bell runs ("this Bell runs Square
production and sandbox, and Clover europe"); the token is used for neither.
For Clover, `location_id` is the merchant id. `subscription` is what
`PushSubscription.toJSON()` gives (other fields in it are ignored); any
other unknown field is refused. The Bell then asks that environment's API,
with the token, once:

- **Square:** `GET /v2/locations/{location_id}` (the token needs the
  `MERCHANT_PROFILE_READ` scope): Square answers only for a location of the
  token's own merchant, and its answer names that merchant.
- **Clover:** `GET /v3/merchants/{location_id}` (the app needs Merchant
  read): Clover answers only for the token's own merchant, and the answer's
  `id` must be the one asked. Nothing else of the answer is read.

Neither follows a redirect. Answered `201`:

```json
{ "registration_id": "br_0f3c…", "point_of_sale": "clover", "environment": "europe", "application_id": "ABCDEFGHIJKLM" }
```

The same subscription registered again keeps its `registration_id`, moved to
the location it names and with its keys replaced, when the token is its own
merchant's in its own integration, or when its keys are the ones it was
registered with (a till signed in to another merchant's account, or moved to
another point of sale or environment). An endpoint alone isn't enough: anyone else is
refused with [`endpoint-registered`](#endpoint-registered), and the
registration is left as it was. A location takes 32 registrations; the same
id at Square and at Clover, or in Square's sandbox and its production, is two
locations. The endpoint must be `https`
(or `http` on the Bell's own machine), never an onion address, and on a
public address unless the Bell allows private ones (a `localhost` name
counts as private). Registrations are limited per address (IPv6 by its
/64): each costs a request to a point of sale.

Problems: [`invalid-request`](#invalid-request),
[`square-refused`](#square-refused), [`clover-refused`](#clover-refused),
[`square-unavailable`](#square-unavailable),
[`clover-unavailable`](#clover-unavailable),
[`too-many-registrations`](#too-many-registrations),
[`endpoint-registered`](#endpoint-registered),
[`payload-too-large`](#payload-too-large), [`rate-limited`](#rate-limited).

### `DELETE /v1/register/{registration_id}`

Takes a registration back: `204`, or [`unknown-registration`](#unknown-registration)
when there is none (a second `DELETE` of the same id). The id is all it takes:
it is 128 random bits, known only to the till that registered.

### `POST /v1/square/{environment}/webhook`

Square's, `/v1/square/production/webhook` or `/v1/square/sandbox/webhook`,
when this Bell serves that environment (otherwise `404`, the body unread).
The `x-square-hmacsha256-signature` header must be Square's: base64 of
HMAC-SHA256, keyed with that environment's subscription's signature key
(`MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY` or
`MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY`), over the notification URL
(`bell.public_uri` followed by that path, exactly as the subscription in the
Developer Console names it) followed by the body: a sandbox webhook sent to
production's URL, or signed with production's key, is refused.
Answered `200` at once; a `payment.created` or `payment.updated` whose payment
is `COMPLETED` then dings each registration in that environment of its
merchant's location, once per payment: a later `payment.updated` of the same payment
(its processing fee filled in) doesn't ding again, and one of a refunded
payment (`refunded_money` there) never dings. Every other event, and a signed
body that doesn't read, is taken and dropped. A restart forgets which
payments rang, so one may ding twice across it.

Problems: [`invalid-signature`](#invalid-signature),
[`payload-too-large`](#payload-too-large).

### `POST /v1/clover/{environment}/webhook`

Clover's, such as `/v1/clover/north_america/webhook` or
`/v1/clover/sandbox/webhook`, when this Bell serves that environment
(otherwise `404`, the body unread). Clover signs nothing: every webhook
carries the app's Auth Code as its `X-Clover-Auth` header, which must be
that environment's, `MONOKULO_BELL_CLOVER_<ENVIRONMENT>_AUTH_CODE`, byte for
byte (compared in constant time). That static header is the whole of the
authentication: whoever holds the code can send the Bell webhooks, which
costs each till one request to Clover, within the merchant's ding share.

With the header, answered `200` at once; then each update whose `objectId` is
a payment's (`P:<id>`) and whose `type` is `CREATE` dings each registration
in that environment of its merchant, once per payment. `UPDATE` (a refund, a tip, a
void, days later) and `DELETE` never ding, nor does any other object (`O:`,
`M:`, …). A batch of another app (`appId` isn't that environment's
`bell.clover.<environment>.application_id`)
is dropped, as is a body that doesn't read.

Without the header, the one thing taken is Clover's **verification
handshake**: a body that is exactly `{"verificationCode": "<UUID>"}` is
answered `200` and the code logged at `info`:

```
INFO monokulo_bell::routes: Clover north_america webhook verification code: 5a237ce7-… (enter it on the Webhooks page of the north_america app's developer dashboard, then Verify)
```

Anything else without the header (any other value, any other body, more
than 1 KiB) is [`invalid-signature`](#invalid-signature) with nothing logged,
so nobody can write into the Bell's log. The operator's steps are
[below](#clover-s-app): start the Bell, press Send Verification Code in
Clover's dashboard, read the code in the log, paste it, Verify, Save, then
subscribe to Payments.

Problems: [`invalid-signature`](#invalid-signature),
[`payload-too-large`](#payload-too-large).

## Dings

`POST` to the subscription's endpoint, once (no queue, no retry), with:

| Header | Value |
| --- | --- |
| `Content-Encoding` | `aes128gcm`: `{"wake":"square"}` or `{"wake":"clover"}` (the payment's point of sale) encrypted to the subscription's keys (RFC 8291) |
| `Urgency` | `high` |
| `TTL` | `60`: a ding a minute late is worth nothing |
| `Topic` | `wake`: a push service holding several for an offline till hands over one |
| `Authorization` | `vapid t=…, k=…` (RFC 8292), signed with the Bell's key, `sub` its `https` address |

A push service answering `404` or `410` has the registration deleted at once;
`bell.registration_failure_limit` dings in a row that it doesn't take (any other
error, or no answer) delete it too. `429` isn't counted: that is the push
service limiting the Bell, not the till's subscription failing. Each
merchant (its integration and its id) is sent at most
`bell.dings_per_merchant_per_minute` dings a minute; the rest are dropped.

## Problems

### invalid-request

`400`: the body isn't what the route takes: not JSON, a field missing or
unknown (`square_access_token` among them), a `point_of_sale` that isn't
`square` or `clover`, an `environment` its point of sale doesn't have, an
integration this Bell doesn't serve (the detail names those it runs), a location ID or token that can't be one, a Square location named
by an alias such as `main` rather than its own ID (Square answers it with
another ID, and webhooks name that one), a Clover merchant Clover answers
another id for, keys that aren't a P-256 point and a 16-byte secret, or an
endpoint the Bell won't send to.

### square-refused

`401`: Square refused the access token (expired, revoked, or without
`MERCHANT_PROFILE_READ`), or the location isn't the token's merchant's.

### clover-refused

`401`: Clover refused the access token (expired, revoked, or the app without
Merchant read), or the merchant isn't the token's.

### square-unavailable

`503`: Square didn't answer, or answered that it couldn't. Try again later.

### clover-unavailable

`503`: Clover didn't answer, or answered that it couldn't (`429`, `5xx`), or
answered with something that isn't a merchant. Try again later.

### unknown-registration

`404`: no such registration: never made, taken back, or deleted after its push
service said the subscription was gone. Register again.

### too-many-registrations

`409`: the location has 32 registrations. Take back one that no longer uses
this Bell.

### endpoint-registered

`409`: the subscription's endpoint is registered for another merchant (or the
same merchant id at another point of sale, or in another environment), and the keys given aren't the
ones it was registered with. Take the old registration back (`DELETE` with
its id), or register with the subscription's own keys.

### invalid-signature

`401`: the webhook isn't its point of sale's for this Bell. For Square, it
isn't signed with this Bell's key for that environment and its URL, and the
detail names the URL the Bell expects the subscription to have; for Clover
(titled "Not from Clover"), its `X-Clover-Auth` header is missing or isn't
that environment's app's Auth Code.
No detail repeats what was sent.

### payload-too-large

`413`: more than the route reads (8 KiB for a registration, 256 KiB for a
webhook).

### rate-limited

`429`: too many registrations from this address this minute. `Retry-After`
(and the body's `retry_after`) says how many seconds to wait.

### internal

`500`: something went wrong on the Bell. Try again in a moment.

## Run one

A Bell serves integrations, a point of sale's environment each: Square
`production` and `sandbox`, Clover `north_america`, `europe`,
`latin_america` and `sandbox`, as many at once as you set up. Each has an
application of its own (the one the tills sign in through) whose webhooks
come to it, and the Bell needs an https address they can reach.

```sh
monokulo-bell --init      # writes ~/.config/monokulo/bell.toml, every setting described
```

In `bell.toml` set the Bell's address and, in a table per integration named
for its environment, its application: an integration is on when its
`application_id` is set. Square's id says which environment it is in, and
must be in that one's table.

```toml
[bell]
public_uri = "https://bell.example.org"

[bell.square.production]
application_id = "sq0idp-…"               # webhooks to https://bell.example.org/v1/square/production/webhook

[bell.square.sandbox]
application_id = "sandbox-sq0idb-…"       # webhooks to …/v1/square/sandbox/webhook

[bell.clover.north_america]
application_id = "ABCDEFGHIJKLM"          # the App ID; webhooks to …/v1/clover/north_america/webhook
```

Each table also takes `api_url`, a stand-in instead of the environment's own
API while developing. The webhook URL of each integration, to enter in its
point of sale's dashboard:

| Integration | Table | Webhook URL, after `bell.public_uri` |
| --- | --- | --- |
| Square production | `[bell.square.production]` | `/v1/square/production/webhook` |
| Square sandbox | `[bell.square.sandbox]` | `/v1/square/sandbox/webhook` |
| Clover North America | `[bell.clover.north_america]` | `/v1/clover/north_america/webhook` |
| Clover Europe | `[bell.clover.europe]` | `/v1/clover/europe/webhook` |
| Clover Latin America | `[bell.clover.latin_america]` | `/v1/clover/latin_america/webhook` |
| Clover sandbox | `[bell.clover.sandbox]` | `/v1/clover/sandbox/webhook` |

The secrets come from the environment only, one per integration, each set
when, and only when, its integration is on (one set for an integration that
is off stops the Bell, as it is most likely a table misnamed):

```sh
export MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY=…    # Square production: its subscription's signature key (Developer Console › Webhooks)
export MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY=…       # Square sandbox: its own subscription's key
export MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE=…   # Clover north_america: the app's Auth Code (developer dashboard › Webhooks)
export MONOKULO_BELL_VAPID_KEY=$(openssl rand -hex 32)  # once; keep it: the tills' subscriptions are made for its public key
monokulo-bell
```

The other Clover environments' are `MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE`,
`MONOKULO_BELL_CLOVER_LATIN_AMERICA_AUTH_CODE` and
`MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE`.

The settings of a Bell of one environment per point of sale stop it, each
naming what replaced it: the tables `[bell.square]` and `[bell.clover]`
(`application_id`, `environment`, `api_url`), their options
(`--bell-square-application-id`, …), and the variables
`MONOKULO_BELL_SQUARE_WEBHOOK_KEY` and `MONOKULO_BELL_CLOVER_AUTH_CODE`
and their `MONOKULAR_BELL_*` names. Until the release after the one that
renamed it, `MONOKULAR_BELL_VAPID_KEY` (from when the Bell was
`monokular-bell`) is still read, with a warning at start; set under both
names with different values, the Bell stops.

It listens on `127.0.0.1:8445` (`bell.listen`): put an https reverse proxy
in front, serving `bell.public_uri`, as the Bell serves no TLS itself; a
proxy on the same machine is trusted for `X-Forwarded-For` by default
(`bell.trusted_proxies`). Registrations are kept in
`~/.local/share/monokulo/bell.db` (`bell.store_path`). Any setting can be
given on the command line too (`monokulo-bell --help`); anything invalid or
missing, or no integration on, stops it before it listens. Stopping it
(SIGTERM) lets requests and dings in flight finish.

### Check before starting

`monokulo-bell --check` reads the options file and the environment and
checks every setting exactly as a start would, then exits without opening
its database, making a directory or binding a port, so it can be run as
another user than the Bell's (a service file's pre-start step, a router's
settings page):

```sh
$ monokulo-bell --check
listens on 127.0.0.1:8445
public address https://bell.example.org
Square production: on, application sq0idp-…, webhooks to https://bell.example.org/v1/square/production/webhook
Square sandbox: on, application sandbox-sq0idb-…, webhooks to https://bell.example.org/v1/square/sandbox/webhook
off: Clover north_america, europe, latin_america and sandbox
VAPID public key BEl6…
registrations in /home/bell/.local/share/monokulo/bell.db
status on 127.0.0.1:8447
OK
```

It says a line for each integration that is on and one for those that are
off, and exits 0 when the Bell would start, the VAPID key still under its
`MONOKULAR_BELL_*` name said first as a `warning:` line. Otherwise it
exits 1 with the words a start would stop with, on stderr (`the Bell
didn't start: an integration's application_id must be set, such as
bell.square.production.application_id …`). No line names a secret.

### Status

The Bell answers `GET /status` on a second listener, `status.listen`
(`127.0.0.1:8447` by default; empty for none), with what it is doing as
JSON:

```sh
$ curl -s http://127.0.0.1:8447/status
{"service":"monokulo-bell","version":"0.1.0","commit":"51dfea123…",
 "started_at":1761700000,"listen":"127.0.0.1:8445","public_uri":"https://bell.example.org",
 "registrations":{"stored":4},
 "points_of_sale":{"square":{"production":"ready","sandbox":"ready"},
                   "clover":{"north_america":"off","europe":"off","latin_america":"off","sandbox":"off"}},
 "last_ding":{"at":1761700100,"outcome":"sent"}}
```

`points_of_sale` says every integration, `ready` or `off`.

`commit` is `null` for a build that didn't name one,
`registrations.stored` (every point of sale's) is `null` when the database
couldn't be read, and `last_ding` is `null` until a ding has been sent
since the start. `outcome` is `sent`, `gone` (the push service says the
subscription is gone, and its registration was deleted) or `failed`. It
never carries a token, an endpoint, a merchant, a location or a payment.

The listener is for this machine alone: `status.listen` takes a loopback
address only, and a request naming any other host is refused (403), so a
web page can't read it through a name pointed at `127.0.0.1`. It is a
listener of its own, never a route on `bell.listen`, so the https proxy in
front of the Bell doesn't publish it.

### TLS

The Bell serves plain HTTP only, on this machine by default: its TLS is
the reverse proxy's in front of it, serving `bell.public_uri` over https.
It uses TLS only as a client, to Square, Clover and the tills' push
services, trusting the machine's certificate store, so that store must be
there (`ca-certificates`, or `ca-bundle` on OpenWrt).

### Square's application

Each environment the Bell serves needs its application (production's in the
Developer Console, the sandbox's under its sandbox) with a webhook
subscription to `{public_uri}/v1/square/production/webhook` or
`{public_uri}/v1/square/sandbox/webhook` for `payment.created` and
`payment.updated`; each subscription's signature key is that environment's
secret. The tills' sign-in asks for `MERCHANT_PROFILE_READ`.

### Clover's app

In the developer dashboard of each Clover environment the Bell serves
(Clover's sandbox and each region are dashboards of their own, each with its
own app):

1. Platform **Web**, so it has REST Configuration: Site URL
   `https://monokular.monokulo.com/clover` (the tills' sign-in returns
   there, an App Link of the Monokular app, as Square's does to `/square`),
   Default OAuth Response **Code**.
2. Permissions: Merchant read and write (write lets the till add the Monero
   payment type), Payments read, Orders read.
3. Start the Bell. Under Webhooks, set the callback URL
   `{public_uri}/v1/clover/<environment>/webhook` (such as
   `/v1/clover/north_america/webhook`) and press **Send Verification
   Code**: the Bell answers and logs `Clover north_america webhook
   verification code: …` at info, naming the environment it came to.
   Paste the code, **Verify**, **Save**, then subscribe to **Payments**.
4. Copy the App ID to `bell.clover.<environment>.application_id` and the
   Auth Code to `MONOKULO_BELL_CLOVER_<ENVIRONMENT>_AUTH_CODE` (such as
   `MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE`), and restart the Bell.

Clover's webhooks carry no signature, only the Auth Code as a header, and
don't say how a payment was paid: keep the code as secret as a key, and know
that every payment at a Clover merchant, card or Monero, dings its tills
(the till asks Clover and finds nothing new for a card).

### Through a push relay

An iPhone till, or an Android one with only Google's services, takes its
dings through a push relay ([`monokulo-push-relay`](../push-relay/)), and a
relay counts pushes per sender, which is the Bell's VAPID key: every
merchant's tills behind it are one sender. A payment dings each till
registered at its location once, so at its busiest a Bell sends about the
payments a minute across all its merchants times the tills at each location;
whoever runs the relay sizes its `limits.per_sender_per_minute` (600 by
default) above that, or the relay answers `429` and those dings are lost: a
ding is never sent again, as a minute late it is worth nothing. Its
`limits.per_handle_per_minute` (60) is per till and is never what a Bell
reaches. So that one busy merchant can't spend the relay's budget for all of
them, the Bell sends each merchant's tills (a Square merchant, a Square
sandbox merchant and a Clover merchant are three) at most `bell.dings_per_merchant_per_minute` dings a
minute (120 by default), across its locations, and drops the rest: keep it
well below the relay's per-sender limit.

### On one machine

To try it against the stand-in Square and the stand-in Clover in this
repository's `tools/`, with any Web Push endpoint on loopback as the till's:

```sh
FAKE_SQUARE_WEBHOOK_URL=http://127.0.0.1:8445/v1/square/sandbox/webhook FAKE_SQUARE_WEBHOOK_KEY=dev \
  python3 -I tools/fake-square/fake_square.py                          # :8738
FAKE_CLOVER_WEBHOOK_URL=http://127.0.0.1:8445/v1/clover/sandbox/webhook \
  FAKE_CLOVER_AUTH_CODE=11111111-2222-3333-4444-555555555555 FAKE_CLOVER_APP_ID=DEVCLOVERAPP1 \
  python3 -I tools/fake-clover/fake_clover.py                          # :8740
MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY=dev \
  MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE=11111111-2222-3333-4444-555555555555 \
  MONOKULO_BELL_VAPID_KEY=$(openssl rand -hex 32) \
  cargo run -p monokulo-bell -- --bell-public-uri http://127.0.0.1:8445 \
  --bell-square-sandbox-application-id sandbox-sq0idb-dev \
  --bell-square-sandbox-api-url http://127.0.0.1:8738 \
  --bell-clover-sandbox-application-id DEVCLOVERAPP1 \
  --bell-clover-sandbox-api-url http://127.0.0.1:8740 --bell-allow-private-endpoints true
curl -X POST localhost:8740/fake/verify    # Clover's handshake: the Bell logs the code
# register a subscription, "environment": "sandbox": Square for L-DEV with the
# token EAAA-dev-…, Clover for MCLOVERDEV001 with the test token fake-clover prints (fake-clover-test-token-0001);
# with no till, "add_tender": true adds the Monero tender as the merchant would
# in Clover's dashboard. Then:
curl -X POST localhost:8738/fake/sale -d '{"items":[["Flat white",2,450]]}'   # dings {"wake":"square"}
curl -X POST localhost:8740/fake/sale -d '{"items":[["Flat white",2,450]],"add_tender":true}'   # dings {"wake":"clover"}
curl -X POST localhost:8740/fake/refund -d '{"payment_id":"…"}'              # a P: UPDATE: no ding
```
