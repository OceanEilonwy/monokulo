# Abuse protection

How monokulo keeps one abusive visitor from slowing everyone else down, on Tor
and on clearnet, without cookies and without requiring JavaScript. Code:
`crates/monokulo/src/abuse/` and `crates/monokulo/src/http/abuse.rs`. Tor
deployment: [`docs/TOR.md`](TOR.md).

## Who is a client

| Where the request comes from | Client identity |
|---|---|
| Monokulo's onion listener (tor, `HiddenServiceExportCircuitID haproxy`) | The Tor circuit (Tor Browser uses one per site per session) |
| Clearnet, directly | The peer address; IPv6 grouped by `/64` |
| Clearnet, via a proxy listed in `abuse.trusted_proxies` | The last address in `X-Forwarded-For` that isn't a trusted proxy |
| A signed-in merchant (session cookie or bearer token) | The user - own limit, never challenged |
| A shop's server with `Authorization: Bearer sk_...` | The store - own limit, never challenged |

The ordinary listener never accepts a PROXY header, and `X-Forwarded-For` is
only believed from a trusted proxy.

## Tiers

Each anonymous client is counted over a rolling minute:

- under `abuse.soft_per_min` (default 60): nothing changes;
- past it: the client must pass a challenge (below), after which it has a
  10-minute pass (held in memory, keyed by the client - no cookie);
- past `abuse.hard_per_min` (default 300): `429` with `Retry-After` for
  everything, pass or not.

`abuse.under_attack` treats every anonymous page and API request as past the
soft limit (streams are unaffected). Signed-in merchants
(`abuse.signed_in_per_min`, 600) and store keys
(`rate_limit.per_store_key_per_min`, 600) only ever get `429` past their limit.
Static files (`/static/...`), webhooks (sent by the engine) and the status
indicator's `/status/summary` poll are never counted.

| | Pages (checkout, share, landing, login, sign-up, status) | JSON API (create order, order status) | Live updates (SSE) |
|---|---|---|---|
| Past soft | "Checking your connection" page: solved by JavaScript, or a 10-second wait without it | `429` + challenge (below) | `429`; `checkout.js` backs off 5 s → 60 s |
| Past hard | `429` page with `Retry-After`, readable without JavaScript | `429` + `Retry-After` | `429` + `Retry-After` |

Open live-update streams are also capped per (client, store) at
`abuse.stream_cap` (16).

## The challenge

A challenge is a signed, short-lived token bound to the client it was issued
to, usable once. The answer is a nonce (a decimal string) such that
`SHA-256(challenge + nonce)` starts with `difficulty` zero bits
(`abuse.challenge_bits`, default 16: about a second in a browser).

### JSON API

Past the soft limit, `POST /pay/{pk}/orders` and
`GET /pay/{pk}/orders/{id}/status` answer:

```http
HTTP/1.1 429 Too Many Requests
Content-Type: application/json
Monokulo-Challenge: <challenge>; difficulty=16
Cache-Control: no-store

{
  "error": "Too many requests from this connection. Solve the challenge and retry with the Monokulo-Proof header.",
  "challenge": {
    "challenge": "<challenge>",
    "difficulty": 16,
    "expires_in": 300,
    "algorithm": "sha256-leading-zero-bits",
    "proof_header": "Monokulo-Proof"
  }
}
```

Retry the same request with the header:

```http
Monokulo-Proof: <challenge>.<nonce>
```

A wrong, expired, replayed or someone else's proof gets a new `429` challenge
whose `error` says why. Past the hard limit the answer is instead:

```http
HTTP/1.1 429 Too Many Requests
Retry-After: 37

{"error": "rate limit exceeded", "retry_after": 37}
```

CORS allows the `Monokulo-Proof` request header and exposes
`Monokulo-Challenge` and `Retry-After` to any site allowed to embed the store,
so a page on another site can solve it. The embed library
(`/static/monokulo-client.js`) does all of this by itself; merchants change
nothing. Server integrations (the WooCommerce plugin) use the store's secret
key and are never challenged.

### Pages

The interstitial is served in place of the page (status `429`,
`Cache-Control: no-store`), works inside a frame, and explains itself
(`role="status"`). With JavaScript, `/static/challenge.js` solves the challenge
with Web Crypto and loads the page again with `?monokulo_proof=<challenge>.<nonce>`.
Without it, a `<meta http-equiv="refresh">` loads it after 10 seconds with
`?monokulo_wait=<token>`, a signed token only valid from 10 seconds after issue.
Either way monokulo grants the pass and redirects (`303`) to the page without
the parameter.

## Settings

All on the admin settings page, under "Abuse protection", with help text and
validation; everything but the onion listener applies immediately.

| Setting | Env var | Default |
|---|---|---|
| `abuse.trusted_proxies` | `MONOKULO_ABUSE_TRUSTED_PROXIES` | (none) |
| `abuse.onion_listener` | `MONOKULO_ABUSE_ONION_LISTENER` | (off) |
| `abuse.soft_per_min` | `MONOKULO_ABUSE_SOFT_PER_MIN` | 60 |
| `abuse.hard_per_min` | `MONOKULO_ABUSE_HARD_PER_MIN` | 300 |
| `abuse.signed_in_per_min` | `MONOKULO_ABUSE_SIGNED_IN_PER_MIN` | 600 |
| `rate_limit.per_store_key_per_min` | `MONOKULO_RATE_LIMIT_PER_STORE_KEY_PER_MIN` | 600 |
| `abuse.stream_cap` | `MONOKULO_ABUSE_STREAM_CAP` | 16 |
| `abuse.challenge_bits` | `MONOKULO_ABUSE_CHALLENGE_BITS` | 16 |
| `abuse.under_attack` | `MONOKULO_ABUSE_UNDER_ATTACK` | false |

Operators (admins) see challenges issued, solved and refused in the last hour,
and whether under-attack mode is on, on the status page.

## Why not Anubis

It needs JavaScript (the checkout must work without it); its pass is a cookie,
which Tor Browser, Safari and Firefox block or partition in a third-party
checkout frame; the embed library's `fetch`, SSE streams, CORS preflights and
server integrations can't solve a challenge page; and it would be another
service in the payment path.
