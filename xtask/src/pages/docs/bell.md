# The Monokular Bell

The Bell turns a point of sale's payment webhooks into dings for the tills
registered at that location (`docs/design.md` §3). It serves Square, Clover,
or both: each point of sale is an adapter, on when the Bell's settings name
an application for it (`[bell.square]`, `[bell.clover]`). A ding is a Web
Push whose only content is an encrypted `{"wake":"square"}` or
`{"wake":"clover"}`: it wakes the till to ask its point of sale for new
payments itself, so a lost, late, repeated or forged one costs one request
to the point of sale at most.

This page is the Bell's API, as a till (or anyone writing one) uses it, and
what each of its errors means. How to run one is in the
[README](../README.md#running-a-bell); the choices behind it are in
[decisions/bell.md](decisions/bell.md).

A point of sale is named by its wire name, the one spelling everywhere:
`square` or `clover`, in a registration's `point_of_sale`, `/v1/info`'s
keys, a ding's `wake`, the webhook path `/v1/<pos>/webhook` and the problems
`<pos>-refused` and `<pos>-unavailable`.

This is the Bell's second API (version 0.2.0). The first, Square's alone,
took `square_access_token` and answered `square_application_id`; those
names are gone, with no shim: no Bell was deployed and the till that spoke
to it was unreleased (decision 44).

## What the Bell sees and keeps

**Square** sends the Bell every webhook of its application, for every
merchant signed in through it, with each payment's amount, card and buyer.
The Bell checks the signature, reads a payment event's `type`,
`merchant_id`, and its payment's `id`, `location_id`, `status` and whether
`refunded_money` is there (never what it says), and nothing else.

**Clover** sends the Bell every webhook of its app, batched: ids and times
only, `{"appId", "merchants": {"<merchant>": [{"objectId", "type", "ts"}]}}`.
The Bell checks the `X-Clover-Auth` header, reads `appId`, each merchant
key, and each update's `objectId` (its id only when it is a payment's,
`P:…`) and `type`, and nothing else. The one value of a body it ever logs is
Clover's verification code, once, for the operator
([below](#post-v1cloverwebhook)).

Neither body is stored, logged or put into an answer. The ids of the payments
it rang, with their point of sale, are kept in memory for 10 minutes (10,000
at most), so each payment rings once, and nowhere else. A merchant's access
token is used for one request to its point of sale at registration and
dropped.

Its own SQLite file (`bell.store_path`) holds one table: per registration,
its point of sale, the merchant and location, the till's push subscription
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
    "square": { "application_id": "sq0idp-…", "environment": "production" },
    "clover": { "application_id": "ABCDEFGHIJKLM", "environment": "north_america" } },
  "vapid_public_key": "BOS-CZ1F…", "version": "0.2.0" }
```

`points_of_sale` has a key for each point of sale this Bell serves, and
none for the others (at least one is there, Square first when both are).
Square's `environment` is `production` or `sandbox`; Clover's `sandbox`,
`north_america`, `europe` or `latin_america`. A till signs in to its point of
sale through the `application_id` given (each sends webhooks only to the
application a merchant signed in through), and makes its push subscription
for `vapid_public_key` (the `applicationServerKey`, or the
`vapid_public_key` it registers with monokulo's push relay): every ding is
signed with that key.

### `POST /v1/register`

One body for every point of sale:

```json
{ "point_of_sale": "square",
  "access_token": "EAAA…",
  "location_id": "L8GWD5R9QB376",
  "subscription": { "endpoint": "https://…", "keys": { "p256dh": "…", "auth": "…" } } }
```

```json
{ "point_of_sale": "clover",
  "access_token": "…",
  "location_id": "CLVMERCHANT01",
  "subscription": { … } }
```

`point_of_sale` is required and must be one this Bell serves; for Clover,
`location_id` is the merchant id. `subscription` is what
`PushSubscription.toJSON()` gives (other fields in it are ignored); any
other unknown field is refused. The Bell then asks the point of sale, with
the token, once:

- **Square:** `GET /v2/locations/{location_id}` (the token needs the
  `MERCHANT_PROFILE_READ` scope): Square answers only for a location of the
  token's own merchant, and its answer names that merchant.
- **Clover:** `GET /v3/merchants/{location_id}` (the app needs Merchant
  read): Clover answers only for the token's own merchant, and the answer's
  `id` must be the one asked. Nothing else of the answer is read.

Neither follows a redirect. Answered `201`:

```json
{ "registration_id": "br_0f3c…", "point_of_sale": "clover", "application_id": "ABCDEFGHIJKLM" }
```

The same subscription registered again keeps its `registration_id`, moved to
the location it names and with its keys replaced, when the token is its own
merchant's at its own point of sale, or when its keys are the ones it was
registered with (a till signed in to another merchant's account, or moved to
another point of sale). An endpoint alone isn't enough: anyone else is
refused with [`endpoint-registered`](#endpoint-registered), and the
registration is left as it was. A location takes 32 registrations; the same
id at Square and at Clover is two locations. The endpoint must be `https`
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

### `POST /v1/square/webhook`

Square's, when this Bell serves Square (otherwise `404`, the body unread).
The `x-square-hmacsha256-signature` header must be Square's: base64 of
HMAC-SHA256, keyed with the subscription's signature key, over the
notification URL (`bell.public_uri` followed by `/v1/square/webhook`, exactly
as the subscription in the Developer Console names it) followed by the body.
Answered `200` at once; a `payment.created` or `payment.updated` whose payment
is `COMPLETED` then dings each Square registration of its merchant's
location, once per payment: a later `payment.updated` of the same payment
(its processing fee filled in) doesn't ding again, and one of a refunded
payment (`refunded_money` there) never dings. Every other event, and a signed
body that doesn't read, is taken and dropped. A restart forgets which
payments rang, so one may ding twice across it.

Problems: [`invalid-signature`](#invalid-signature),
[`payload-too-large`](#payload-too-large).

### `POST /v1/clover/webhook`

Clover's, when this Bell serves Clover (otherwise `404`, the body unread).
Clover signs nothing: every webhook carries the app's Auth Code as its
`X-Clover-Auth` header, which must be `MONOKULAR_BELL_CLOVER_AUTH_CODE` byte
for byte (compared in constant time). That static header is the whole of the
authentication: whoever holds the code can send the Bell webhooks, which
costs each till one request to Clover, within the merchant's ding share.

With the header, answered `200` at once; then each update whose `objectId` is
a payment's (`P:<id>`) and whose `type` is `CREATE` dings each Clover
registration of its merchant, once per payment. `UPDATE` (a refund, a tip, a
void, days later) and `DELETE` never ding, nor does any other object (`O:`,
`M:`, …). A batch of another app (`appId` isn't `bell.clover.application_id`)
is dropped, as is a body that doesn't read.

Without the header, the one thing taken is Clover's **verification
handshake**: a body that is exactly `{"verificationCode": "<UUID>"}` is
answered `200` and the code logged at `info`:

```
INFO monokular_bell::routes: Clover webhook verification code: 5a237ce7-… (enter it on the Webhooks page of the app's developer dashboard, then Verify)
```

Anything else without the header (any other value, any other body, more
than 1 KiB) is [`invalid-signature`](#invalid-signature) with nothing logged,
so nobody can write into the Bell's log. The operator's steps are in the
README ([Running a Bell](../README.md#running-a-bell)): start the Bell, press
Send Verification Code in Clover's dashboard, read the code in the log,
paste it, Verify, Save, then subscribe to Payments.

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
merchant (its point of sale and its id) is sent at most
`bell.dings_per_merchant_per_minute` dings a minute; the rest are dropped.

## Problems

### invalid-request

`400`: the body isn't what the route takes: not JSON, a field missing or
unknown (`square_access_token` among them), a `point_of_sale` that isn't
`square` or `clover`, or isn't one this Bell serves (the detail names those
it runs), a location ID or token that can't be one, a Square location named
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
same merchant id at another point of sale), and the keys given aren't the
ones it was registered with. Take the old registration back (`DELETE` with
its id), or register with the subscription's own keys.

### invalid-signature

`401`: the webhook isn't its point of sale's for this Bell. For Square, it
isn't signed with this Bell's key for its URL, and the detail names the URL
the Bell expects the subscription to have; for Clover (titled "Not from
Clover"), its `X-Clover-Auth` header is missing or isn't the app's Auth Code.
No detail repeats what was sent.

### payload-too-large

`413`: more than the route reads (8 KiB for a registration, 256 KiB for a
webhook).

### rate-limited

`429`: too many registrations from this address this minute. `Retry-After`
(and the body's `retry_after`) says how many seconds to wait.

### internal

`500`: something went wrong on the Bell. Try again in a moment.
