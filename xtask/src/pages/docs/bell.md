# The Monokular Bell

The Bell turns a Square application's payment webhooks into dings for the
tills registered at that location (`docs/design.md` §3). A ding is a Web Push
whose only content is an encrypted `{"wake":"square"}`: it wakes the till to
ask Square for new payments itself, so a lost, late, repeated or forged one
costs one request to Square at most.

This page is the Bell's API, as a till (or anyone writing one) uses it, and
what each of its errors means. How to run one is in the
[README](../README.md#running-a-bell); the choices behind it are in
[decisions/bell.md](decisions/bell.md).

## What the Bell sees and keeps

Square sends the Bell every webhook of its application, for every merchant
signed in through it, with each payment's amount, card and buyer. The Bell
checks the signature, reads a payment event's `type`, `merchant_id`, and its
payment's `id`, `location_id`, `status` and whether `refunded_money` is there
(never what it says), and nothing else: no body is stored, logged or put into
an answer. The ids of the payments it rang are kept in memory for 10 minutes
(10,000 at most), so each payment rings once, and nowhere else. A merchant's access token is used for one
request to Square at registration and dropped.

Its own SQLite file (`bell.store_path`) holds one table: per registration, the
merchant and location, the till's push subscription (endpoint and keys), when
it was made, the last ding its push service took, and how many in a row it
didn't. A log line names a registration id and an endpoint's host at most.

## Routes

Request and answer bodies are JSON. Errors are `application/problem+json`
(RFC 9457), their `type` this page's section for the problem.

### `GET /v1/info`

What a till needs to choose this Bell:

```json
{ "square_application_id": "sq0idp-…", "environment": "production",
  "vapid_public_key": "BOS-CZ1F…", "version": "0.1.0" }
```

The till signs in to Square through `square_application_id` (Square sends
webhooks only to the application a merchant signed in through), and makes its
push subscription for `vapid_public_key` (the `applicationServerKey`, or the
`vapid_public_key` it registers with monokulo's push relay): every ding is
signed with that key.

### `POST /v1/register`

```json
{ "square_access_token": "EAAA…",
  "location_id": "L8GWD5R9QB376",
  "subscription": { "endpoint": "https://…", "keys": { "p256dh": "…", "auth": "…" } } }
```

`subscription` is what `PushSubscription.toJSON()` gives (other fields in it
are ignored); any other unknown field is refused. The Bell asks Square
`GET /v2/locations/{location_id}` with the token (which needs the
`MERCHANT_PROFILE_READ` scope): Square answers only for a location of the
token's own merchant, and its answer names that merchant. Answered `201`:

```json
{ "registration_id": "br_0f3c…", "square_application_id": "sq0idp-…" }
```

The same subscription registered again keeps its `registration_id`, moved to
the location it names and with its keys replaced, when the token is its own
merchant's, or when its keys are the ones it was registered with (a till
signed in to another merchant's account). An endpoint alone isn't enough:
anyone else is refused with [`endpoint-registered`](#endpoint-registered),
and the registration is left as it was. A location takes 32
registrations. The endpoint must be `https` (or `http` on the Bell's own
machine), never an onion address, and on a public address unless the Bell
allows private ones (a `localhost` name counts as private). Registrations
are limited per address (IPv6 by its /64): each costs a request to Square.

Problems: [`invalid-request`](#invalid-request), [`square-refused`](#square-refused),
[`square-unavailable`](#square-unavailable),
[`too-many-registrations`](#too-many-registrations),
[`endpoint-registered`](#endpoint-registered),
[`payload-too-large`](#payload-too-large), [`rate-limited`](#rate-limited).

### `DELETE /v1/register/{registration_id}`

Takes a registration back: `204`, or [`unknown-registration`](#unknown-registration)
when there is none (a second `DELETE` of the same id). The id is all it takes:
it is 128 random bits, known only to the till that registered.

### `POST /v1/square/webhook`

Square's. The `x-square-hmacsha256-signature` header must be Square's: base64
of HMAC-SHA256, keyed with the subscription's signature key, over the
notification URL (`bell.public_uri` followed by `/v1/square/webhook`, exactly
as the subscription in the Developer Console names it) followed by the body.
Answered `200` at once; a `payment.created` or `payment.updated` whose payment
is `COMPLETED` then dings each registration of its merchant's location, once
per payment: a later `payment.updated` of the same payment (its processing fee
filled in) doesn't ding again, and one of a refunded payment (`refunded_money`
there) never dings. Every other event, and a signed body that doesn't read, is
taken and dropped. A restart forgets which payments rang, so one may ding
twice across it.

Problems: [`invalid-signature`](#invalid-signature),
[`payload-too-large`](#payload-too-large).

## Dings

`POST` to the subscription's endpoint, once (no queue, no retry), with:

| Header | Value |
| --- | --- |
| `Content-Encoding` | `aes128gcm`: `{"wake":"square"}` encrypted to the subscription's keys (RFC 8291) |
| `Urgency` | `high` |
| `TTL` | `60`: a ding a minute late is worth nothing |
| `Topic` | `wake`: a push service holding several for an offline till hands over one |
| `Authorization` | `vapid t=…, k=…` (RFC 8292), signed with the Bell's key, `sub` its `https` address |

A push service answering `404` or `410` has the registration deleted at once;
`bell.registration_failure_limit` dings in a row that it doesn't take (any other
error, or no answer) delete it too. `429` isn't counted: that is the push
service limiting the Bell, not the till's subscription failing.

## Problems

### invalid-request

`400`: the body isn't what the route takes: not JSON, a field missing or
unknown, a location ID or token that can't be one, a location named by an
alias such as `main` rather than its own ID (Square answers it with another
ID, and webhooks name that one), keys that aren't a P-256 point and a 16-byte
secret, or an endpoint the Bell won't send to.

### square-refused

`401`: Square refused the access token (expired, revoked, or without
`MERCHANT_PROFILE_READ`), or the location isn't the token's merchant's.

### square-unavailable

`503`: Square didn't answer, or answered that it couldn't. Try again later.

### unknown-registration

`404`: no such registration: never made, taken back, or deleted after its push
service said the subscription was gone. Register again.

### too-many-registrations

`409`: the location has 32 registrations. Take back one that no longer uses
this Bell.

### endpoint-registered

`409`: the subscription's endpoint is registered for another Square merchant,
and the keys given aren't the ones it was registered with. Take the old
registration back (`DELETE` with its id), or register with the subscription's
own keys.

### invalid-signature

`401`: the webhook isn't signed by Square with this Bell's key for its URL. The
detail names the URL the Bell expects the subscription to have.

### payload-too-large

`413`: more than the route reads (8 KiB for a registration, 256 KiB for a
webhook).

### rate-limited

`429`: too many registrations from this address this minute. `Retry-After`
(and the body's `retry_after`) says how many seconds to wait.

### internal

`500`: something went wrong on the Bell. Try again in a moment.
