# The Monokular Bell: design

The Bell's design note. It was the "The Monokular Bell" part of §3
(Wake-ups) of monokular's `docs/design.md`, and moved here on 2026-10-29
with the Bell itself (from monokular's `crates/monokular-bell` to
`crates/monokulo-bell`, its history kept: `docs/decisions/bell.md` 59 and
on). The rest of that document (the sale flow, the app, the push design it
shares with monokulo) stays in the monokular repository. The Bell's API and
how to run one are on the docs site, <https://docs.monokulo.com/bell/>; the
choices behind it are in [decisions/bell.md](decisions/bell.md).

A till waits for its next look at the point of sale unless something wakes
it: the Bell is that something for a Square or Clover counter.


A small service that turns a POS's webhooks into dings for the right tills.
It lives in this repository as `crates/monokulo-bell` (a library and the
`monokulo-bell` binary), beside the gateway and never inside it: it is
POS-specific and app-side, and the monokulo binary depends on nothing of it
(`lint.yml` fails the build if it ever does). It builds on the `push`,
`live-settings`, `sqlite`, `rate-limit`, `abuse`, `outbound`,
`service-runtime` and `telemetry` crates here, as path dependencies, so it is
configured, stores, limits and logs the way monokulo does. Its API is on the
docs site, [The Monokular Bell](https://docs.monokulo.com/bell/)
(`xtask/src/pages/docs/bell.md`).

```text
a POS's payment webhook ──▶ Bell: verify it (Square: signature; Clover: X-Clover-Auth),
                                  read the merchant, location and payment id, drop the body
Bell ──Web Push (empty, encrypted)──▶ each till registered in that integration at that location
        (UnifiedPush distributor directly, or APNs/FCM via the push relay)
```

- **One Bell, any of its points of sale.** Each POS is an adapter: a
  variant of one `PointOfSale` enum (`square`, `clover`, the wire name
  everywhere) and a module (`square.rs`, `clover.rs`) that checks the POS's
  webhooks, reads their routing fields and proves a registration with the
  till's token. An adapter runs for each integration, a POS's environment
  (Square `production`, `sandbox`; Clover `north_america`, `europe`,
  `latin_america`, `sandbox`), that is on: its settings table names an
  application. An operator turns on the ones they have developer apps for,
  as many at once as they like: Square production and its sandbox side by
  side, say, or two Clover regions (decision 75).
- **Dings go over Web Push**, exactly like monokulo's own pushes: the
  till hands the Bell a Web Push subscription, so the Bell never talks to
  Apple or Google and UnifiedPush needs no relay. The payload is an
  encrypted `{"wake":"square"}` or `{"wake":"clover"}`, the POS to ask;
  `Urgency: high`, `TTL: 60`, `Topic: wake` so a burst collapses into one.
- **One application per integration.** Square and Clover send webhooks to
  the application a merchant signed in through, so a Bell and the tills that
  use it share one for each integration it serves (until 2026-10-30 a Bell
  served one environment per POS: decision 75 replaces that). Each
  environment's merchant ids are its own: a sandbox merchant is never a
  production one, so everything a registration is matched by is keyed by
  its integration. The project's Bell
  (`bell.monokulo.com`) uses the project's applications. A self-hosted Bell
  is configured with its own, and tills that connect to it sign in through
  those (the app asks the cashier to sign in again when they switch).
- **What the Bell sees:** Square's webhook bodies, which include each
  payment's amount and card details, and Clover's, which hold ids and times
  only, for every merchant signed in through its applications. It reads the
  merchant, the location (Clover: the merchant), the payment's id, and for
  Square its status and whether it was refunded, then drops the body: none
  of it is stored or logged (Clover's one-off verification code aside,
  logged for the operator). It remembers the payments it rang, in memory
  for ten minutes, so a payment's later updates don't ding again: on iPhone
  and iPad a ding is an alert. Square's payment dings when it completes;
  Clover's when it is created (an update could be a refund days later), and
  as Clover's webhook doesn't say how a payment was paid, every payment at a
  Clover merchant dings and the till sorts it out. It never sees orders and
  holds no POS token. This is why the Bell is optional and anyone can run
  their own.

Registration, from the app, one body for every POS:

```http
POST https://bell.monokulo.com/v1/register
{ "point_of_sale": "square" | "clover",
  "environment": "production" | "sandbox"            (Square)
               | "north_america" | "europe" | "latin_america" | "sandbox"   (Clover),
  "access_token": "…",                    (used once, never stored: Square
                                            GET /v2/locations/{location_id}, whose
                                            merchant_id proves the token and the location;
                                            Clover GET /v3/merchants/{location_id}, whose
                                            id must be the one asked)
  "location_id": "L…" | "<Clover merchant id>",
  "subscription": { "endpoint", "keys": { "p256dh", "auth" } } }
→ 201 { "registration_id": "br_…", "point_of_sale": "…", "environment": "…", "application_id": "…" }
DELETE /v1/register/{registration_id}
GET    /v1/info → { "points_of_sale": { "square": { "production": { "application_id" },
                                                   "sandbox": { "application_id" } },
                                        "clover": { "europe": { "application_id" } } },
                    "vapid_public_key", "version" }      (the integrations that are on)
POST   /v1/{pos}/{environment}/webhook, such as /v1/square/production/webhook
       (only for the integrations that are on)
```

The token check stops anyone from subscribing to dings for a business they
don't control (which would tell them when it takes payments). An endpoint
already registered moves only for its own merchant in its own integration, or for a
caller giving the subscription's own auth secret: an endpoint is no secret
(push services and distributors see it), so knowing one doesn't take a
till's dings away.

Clover's webhook is a batch, `{"appId", "merchants": {"<mId>": [{"objectId":
"P:<payment>", "type": "CREATE", "ts"}]}}`, authenticated by the app's Auth
Code in `X-Clover-Auth` alone (Clover signs nothing). Setting the webhook URL
up, Clover first posts `{"verificationCode": "<uuid>"}` without the header:
the Bell answers `200` and logs the code for the operator to enter in
Clover's dashboard; anything else without the header is refused, unlogged.

Storage: its own SQLite file with `registrations(id, point_of_sale,
environment, merchant_id, location_id, endpoint, p256dh, auth, created_at,
last_ok_at, failures)` (`point_of_sale` added by migration 0002, the first
Bell's rows Square's; `environment` by 0003, the rows before it in the
environment their POS defaulted to, Square's `production` and Clover's
`north_america`). Registrations whose endpoint answers 404/410 are deleted, the
same rule as monokulo's push targets, and so are those whose push service
fails `bell.registration_failure_limit` dings in a row. A location takes 32,
counted within its integration.

Settings (TOML options file plus CLI, invalid values stop the process):
`bell.listen`, `bell.public_uri` (each integration's webhook URL is it
followed by `/v1/<pos>/<environment>/webhook`; Square's signature covers
it), `bell.store_path`, a table per integration whose name is its
environment, `[bell.square.production]`, `[bell.square.sandbox]`,
`[bell.clover.north_america]`, `[bell.clover.europe]`,
`[bell.clover.latin_america]` and `[bell.clover.sandbox]`, each with
`application_id` (a Square id's prefix must match its table: `sq0idp-` in
production, `sandbox-sq0idb-` in the sandbox) and `api_url` (a stand-in
instead of the environment's API), `bell.registration_failure_limit`,
`bell.registrations_per_minute`, `bell.dings_per_merchant_per_minute` (120:
a merchant's tills, across its locations, get that many dings a minute and
the rest are dropped, so one merchant can't spend the push relay's
per-sender budget, which every merchant behind the Bell shares as one VAPID
key; a merchant is its integration and its id), `bell.trusted_proxies`,
`bell.allow_private_endpoints`, `bell.timeout_ms`. An integration is on iff
its `application_id` is set, and its secret must be set then and only then;
at least one is on. Secrets from the environment only, one per integration,
`MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY` and
`MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY` (each Square subscription's
signature key), `MONOKULO_BELL_CLOVER_<ENVIRONMENT>_AUTH_CODE` for
`NORTH_AMERICA`, `EUROPE`, `LATIN_AMERICA` and `SANDBOX` (each Clover app's
Auth Code), and `MONOKULO_BELL_VAPID_KEY` (32 bytes as 64 hex digits;
`/v1/info` gives its public key). The settings of a Bell of one environment
per POS (`[bell.square]`, `[bell.clover]`, `MONOKULO_BELL_SQUARE_WEBHOOK_KEY`,
`MONOKULO_BELL_CLOVER_AUTH_CODE`, their `MONOKULAR_BELL_*` names) stop it,
naming what replaced them (decision 76).

In the app: **Settings › Wake-ups**: "Timer only" (default), "Monokular Bell"
(the build's `-Pmonokular.bell.url`, `bell.monokulo.com` by default), or "My
own Bell" with its address. Built on Android (monokular's
`docs/decisions/android.md`, 90–99 and on): picking
a Bell reads its `/v1/info` and registers for the till's point of sale and
the environment it signed in to: a Bell without that integration refuses the
till ("it doesn't ring Clover tills"), and if the Bell's application for it
differs, the app asks for a new sign-in
through that application (the working sign-in stands until the new one
finishes). It subscribes through the UnifiedPush app installed (ntfy, say;
none installed says so, and the till stays on the timer) for the Bell's VAPID
key, and registers. A status card under the choices says how often the till
checks the point of sale, where the Bell stands and when the last wake-up
came; the timer runs at 30 s only while the Bell is connected, and drops back
to 2 s within a tick when it isn't, or when the timer finds a sale the Bell
didn't ring for. A ding wakes the running shift's check; outside a shift it
does nothing. Signing out of the point of sale, choosing Timer only and
disconnecting take the registration back first.
