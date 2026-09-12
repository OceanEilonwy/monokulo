# MoneroPay Cloud — WooCommerce MVP Roadmap

Status: planning document, no code yet. This lays out an ordered path from the
current self-hosted `moneropay-core` CLI tool to a hosted service that a
merchant can sign up for and get a working "Pay with Monero" option in their
WooCommerce checkout in a handful of clicks.

## 0. Decisions already made

These came out of discussion before this document was written, and everything
below is built on top of them:

- **Hosted, multi-tenant SaaS.** We (not each individual merchant) run the
  server and the Monero node. Signup means a real account on infrastructure we
  operate — this is what makes "one click" possible at all; self-hosting can
  never be one click because there's always a box to provision first.
- **Node**: start on a single curated public node for the hosted service, same
  as `--init`'s curated-node list already offers self-hosters. Revisit (likely
  to "we run our own node") once there's real usage — this is a conscious,
  temporary trust tradeoff, not a permanent architecture decision.
- **Auth**: email + password for the MVP. Social login (Google/GitHub) is a
  fast-follow, not a blocker.

## 1. Guiding principle: don't paint Shopify into a corner

You asked explicitly not to make decisions now that make Shopify hard later.
The single most important structural choice in this plan is built around
that: **split the system into an engine and a control plane, and keep the
control plane platform-agnostic.**

- **The engine** is `moneropay-core` exactly as it exists today — tenants,
  orders, payments, webhooks, the scanner, the checkout page. It already has
  no idea what WooCommerce or Shopify are, and it should stay that way. It
  already supports hosted multi-tenant deployment as a first-class case
  (`docs/DESIGN.md` §4) — a hosted instance is just an instance with tenants
  created at runtime instead of one bootstrapped from `[wallet]`.
- **The control plane** is a new, separate service ("MoneroPay Cloud" in this
  doc) that owns *accounts* and *store connections*. It talks to the engine's
  existing admin API (`POST /api/v1/admin/tenants`, webhook registration,
  etc.) as a client — it does not reach into the engine's database or add new
  engine concepts. Its core data model is deliberately generic:

  ```
  users(id, email, password_hash, created_at)
  store_connections(id, user_id, platform, site_url, tenant_public_key,
                     tenant_secret_token_encrypted, moneropay_endpoint, created_at)
  ```

  `platform` is `"woocommerce"` today. Adding Shopify later means adding
  `"shopify"` as a second value and a second adapter — not touching the
  engine, not touching the `users` table, not touching how tenants are
  created. The connect flow described in Stage 5 is written generically for
  exactly this reason (it's phrased as "a platform sends us a return URL and
  gets a tenant back," which is true of any platform).
- **Each platform gets its own thin adapter** — the WooCommerce plugin now,
  a Shopify app later — that talks to the engine's *public* API directly at
  runtime (order creation, checkout redirect, webhook receipt), and to the
  control plane only during the one-time connect flow. This mirrors
  `docs/DESIGN.md` §14's client-library design (thin edge, all real logic
  server-side) and is why Stage 4 below deliberately picks the *redirect to
  the existing hosted checkout page* integration style rather than embedding
  a new custom UI in WooCommerce's checkout: it's the same shape a future
  Shopify Offsite Payment Extension would need (redirect the buyer to an
  app-hosted payment page), so building it once for WooCommerce is also a
  dry run for Shopify's certified extension model.

If you disagree with "separate control-plane service" as the split point,
that's the one architectural call in this doc I'd flag as worth a second look
before Stage 2 starts — everything downstream assumes it.

## 2. Current state vs. target

What already exists and needs no new engine code:

- Tenant creation, order lifecycle, payment matching, reorg/double-spend
  handling, webhook delivery — all implemented and tested (`src/scanner.rs`,
  `src/store.rs`, `src/webhook_delivery.rs`).
- `POST /api/v1/admin/tenants` already accepts a wallet's view key + public
  spend key and returns `{tenant_id, public_key, secret_token}` — this *is*
  the "create a hosted account" primitive; it just has no signup UI or email
  identity wrapped around it yet.
- The hosted-vs-self-hosted split is already a config-time decision (`[wallet]`
  present or absent), not a code fork.
- `/pay/v1/{pk}/{payment_id}` is already a complete, styled, working checkout
  page with QR code, live status, double-spend banner.

What's missing, all of which this plan builds:

- Anyone can call `POST /api/v1/admin/tenants` right now with no signup at
  all — see Stage 1's note on this.
- No user accounts, no dashboard, no persistent notion of "this merchant."
- No WooCommerce integration of any kind.
- No live exchange-rate provider (only `"fixed"`, i.e. hand-entered rates).
- `PlainKeyCustody` (the only `KeyCustody` implementation) keeps every
  tenant's view key in plaintext in one process's memory — a real
  consideration once "hosted" means *many unrelated merchants* on the same
  box, flagged explicitly in `docs/DESIGN.md` §6.1 and revisited in Stage 9.

## 3. Ordered plan

### Stage 1 — Stand up the hosted engine

Deploy `moneropay-core` itself, unmodified, as a running service:

- Provision the box (or VM), point `[monero_node]` at the chosen curated
  node, run `moneropay-core --init` once to produce the config (no `[wallet]`
  section — hosted mode creates tenants at runtime), start it under a
  process supervisor (systemd unit is enough at this scale).
- TLS termination and the public domain (`https://pay.<yourdomain>` or
  similar — used as `moneropay_endpoint` in the control plane's data model
  above).
- **Network-isolate the admin API.** Today `POST /api/v1/admin/tenants` is
  intentionally open (DDoS-layer-only, per `docs/DESIGN.md` §10.1/§12) —
  correct for the self-hosted case, where "the operator" and "the person
  hitting the API" are the same trusted party. In hosted mode that's no
  longer true: a stranger could create tenants directly, bypassing
  signup/accounts entirely, which defeats the point of building a control
  plane at all. Fix by binding `/api/v1/admin/*` to a private interface (or a
  reverse-proxy rule) reachable only from the control plane's network, while
  `/api/v1/t/{pk}/...` and `/pay/v1/...` stay public exactly as designed —
  no engine code change, purely a deployment-topology decision.
- Basic monitoring: process up/down, node sync height, disk space (the SQLite
  file), webhook delivery queue depth.

This stage is almost entirely ops work, not new code — the payoff of the
engine already having been designed for multi-tenancy.

### Stage 2 — Control-plane service: accounts + store connections

New service, new small database (schema in §1). Responsibilities:

- **Signup/login**: email + password, argon2 hashing (the engine already
  depends on `argon2` for its own secret hashing — reuse the same crate/
  parameters rather than picking a second one), sessions via a signed cookie
  or a server-side session table (either is fine at this scale; a session
  table is easier to force-revoke, which matters once "delete my account"
  exists).
- **Store connection abstraction**: a generic "start a connect flow for
  platform X, site Y" endpoint and a generic "finish it and hand back
  credentials" endpoint (fleshed out fully in Stage 5, but the *shape* of
  these two endpoints should exist before any WooCommerce-specific code is
  written, precisely so Shopify can reuse them later).
- **Tenant provisioning**: wraps the engine's existing `POST
  /api/v1/admin/tenants` — takes the wallet fields a merchant enters (same
  ones the CLI wizard already collects: primary address, private view key,
  public spend key, network) plus store metadata, calls the engine, stores
  the resulting `pk_`/`sk_` against the `store_connections` row.
  `sk_` at rest: encrypted with a key held by the control plane (a KMS key or
  an environment-provided secret, not committed anywhere) — it's used
  server-to-server (webhook registration, later account-management features),
  never re-shown to the merchant after the initial connect flow.
- Recommend building this in Rust/axum, matching the engine — lets it reuse
  `argon2`, keeps one toolchain for the team, and the control plane genuinely
  is a small service (a handful of tables and endpoints), not a reason to
  bring in a second stack. Flagging this as a preference, not a hard
  requirement — a different stack would work too if there's a reason to
  prefer one.

### Stage 3 — Dashboard UI

The web surface a merchant actually sees:

- Signup/login pages.
- "Connect a store" — for the MVP, effectively one button ("Connect
  WooCommerce"), but written as a list so a second platform is an entry, not
  a redesign.
- A wallet-connection form (address + view key + public spend key,
  currency/threshold defaults) — this is the one inherently non-custodial
  step that a competitor holding customer funds doesn't have, and it's worth
  keeping the copy on this page honest about *why* it's needed (never
  pretend it can go away).
- Order list and detail (wrapping `GET /api/v1/admin/tenant/orders` and the
  detail route) and webhook management (list/rotate) — this is the GUI that
  replaces `local_admin.rs`'s CLI flags (`--show-tenant`, `--rotate-secret`)
  for anyone who isn't comfortable on a terminal, which was one of the
  original gaps identified versus Shop Pay.

### Stage 4 — WooCommerce plugin, payment-processing core

The plugin itself, buildable and testable against a local/dev engine
instance before Stages 2–3 (control plane) even exist — it only needs a
`pk_` to talk to the public order API, and account/connect wiring lands in
Stage 5.

- `class WC_Gateway_MoneroPay extends WC_Payment_Gateway`, registered the
  normal WooCommerce way (`woocommerce_payment_gateways` filter).
- **Integration style: redirect to the engine's existing hosted checkout
  page, not a custom in-checkout widget.** `process_payment( $order_id )`
  calls `POST /api/v1/t/{pk}/orders` (server-side, from PHP, using the
  stored `pk_` — no browser CORS/`allowed_origins` concern at all here,
  since the call never leaves the merchant's server) with the WooCommerce
  order's total and currency, then returns WooCommerce's normal "redirect
  the customer to this URL" result pointing at
  `/pay/v1/{pk}/{payment_id}`. This is deliberately the simplest possible
  integration (the same pattern as classic PayPal Standard-style gateways),
  reuses 100% of the already-built and already-tested checkout page (QR
  code, live status, double-spend banner), and needs zero new customer-facing
  UI. It's also, not coincidentally, the same shape a Shopify Offsite Payment
  Extension needs (§1) — the harder polish (an in-checkout widget instead of
  a redirect) is a fast-follow, not an MVP requirement, once this exists.
- `merchant_order_id` on the created order is the WooCommerce order ID, so
  the webhook payload (Stage 6) can map back to a WooCommerce order without
  a separate lookup table.
- Store `pk_`/`sk_`/`endpoint` in WooCommerce's own gateway settings
  (`WC_Payment_Gateway`'s standard settings storage, i.e. `wp_options`) — the
  same trust boundary every other WooCommerce payment gateway plugin already
  uses for its API keys (Stripe's secret key lives the same way); no new
  security model needed here.

### Stage 5 — WooCommerce plugin, one-click connect flow

This is the actual answer to "OAuth-style one-click install," and it's worth
being precise about what it is *not*: it is **not** WooCommerce's own
`wc-auth/v1/authorize` mechanism. That endpoint grants a *third party* access
to a store's own WooCommerce REST API (orders/products) — the direction you'd
need if MoneroPay wanted to read WooCommerce's data remotely. We don't: the
plugin runs *inside* WordPress and already has full native access to
everything it needs (order objects, hooks) with no REST API keys involved.
The actual mechanism, closer to how Stripe's or WooCommerce Payments' own
"Connect" buttons work:

1. Merchant installs and activates the plugin (Stage 7 covers making this
   itself one click). Settings screen shows a single "Connect your Monero
   wallet" button, gateway disabled until connected.
2. Clicking it redirects the merchant's browser to
   `https://cloud.moneropay.example/connect/woocommerce?site_url=<their store>&return_url=<their wp-admin settings page>&nonce=<random>`.
   The plugin generates and stores the nonce locally (a WP option) before
   redirecting, for CSRF/replay protection on the way back.
3. On our dashboard: sign up or log in if not already (Stage 2/3).
4. Dashboard shows "Connect Monero payments for `<site_url>`" and the wallet
   form from Stage 3. On submit, the control plane calls the engine's
   `POST /api/v1/admin/tenants`, gets back `{tenant_id, pk_, sk_}`, registers
   a webhook (`POST /api/v1/admin/tenant/webhooks`, pointed at a URL the
   plugin will define — see Stage 6) using that `sk_`, and stores the
   `store_connections` row.
5. Redirects the browser back to `return_url` carrying a short-lived,
   single-use, signed connect token — **not** the raw `sk_` — to avoid a
   secret ever sitting in a browser history, a referrer header, or an access
   log.
6. The plugin, on receiving that redirect, makes a *server-to-server* call
   (PHP `wp_remote_post`, not the browser) to
   `POST https://cloud.moneropay.example/connect/woocommerce/finish` with
   the token, gets back the real `pk_`/`sk_`/`endpoint`/webhook signing
   secret in the response body, saves them into its own settings, verifies
   the stored nonce matches, and flips the gateway to enabled.

End-to-end merchant-visible steps: install plugin → click Connect → sign
up/log in on our site → paste wallet keys once → done. That's the ceiling on
"one click" that a genuinely non-custodial system can offer — the one step a
custodial competitor skips (entering your own wallet) is real and should stay
visible, not disguised.

### Stage 6 — Order status sync back into WooCommerce

- The plugin defines a normal WordPress URL via WooCommerce's
  `woocommerce_api_{$this->id}` hook (e.g.
  `https://theirsite.com/wc-api/moneropay_webhook`) — no WooCommerce REST API
  keys needed, this is just a plain endpoint the plugin itself owns and
  handles, registered as the webhook URL back in Stage 5 step 4.
- On receipt: verify `X-MoneroPay-Signature` (HMAC-SHA256 against the stored
  signing secret, per `docs/DESIGN.md` §11), then map event → WooCommerce
  order status: `order.paid`/`order.overpaid` → `processing` (or
  `completed`, merchant's choice), `order.expired` → `cancelled`,
  `order.double_spend_detected` → an order note + hold for manual review
  (never auto-cancel on this alone — it's informational per the engine's own
  design, §7.5/§7.6).
- Dedupe on `event_id` (store the last few seen IDs, e.g. as order meta) —
  the engine's webhook delivery is documented at-least-once, so the handler
  must be idempotent regardless of how reliable delivery turns out to be in
  practice.
- End-to-end test this against the engine's existing stagenet e2e
  infrastructure (`e2e/stagenet-wallets.json`, `e2e/demo-shop/`) plus a local
  WordPress/WooCommerce dev environment (`wp-env` is the standard tool for
  this) — send a real stagenet payment through a real WooCommerce checkout
  before calling this stage done.

### Stage 7 — Distribution

- Publish the plugin to the wordpress.org plugin directory. This is free and
  has no partner-approval gate (unlike Shopify) beyond directory guidelines —
  it's what makes "search WooCommerce Marketplace/wp-admin, click Install"
  possible at all.
- A direct deep link from our own marketing site
  (`wp-admin/plugin-install.php?tab=plugin-information&plugin=<slug>`) that,
  for a merchant already logged into their own wp-admin, lands directly on
  the plugin's "Install Now" button — the closest thing to a true one-click
  link WordPress's own model allows for a third party.

### Stage 8 — Exchange-rate automation (parallel track, not blocking)

`docs/DESIGN.md` §13 already sketches pluggable providers
(`haveno | kraken | coingecko | fixed`); only `"fixed"` exists today. A
WooCommerce store commonly needs several currencies without a human typing
in a rate — implement at least one live provider (coingecko is the simplest
to start with) before or shortly after Stage 7's public launch. Doesn't block
the plugin working — a hosted instance could launch with a couple of
hand-maintained rates and swap the provider under it later with zero plugin
changes, since the plugin never sees exchange rates directly.

### Stage 9 — Hosted-specific hardening (gate before public launch, not before private beta)

- `docs/DESIGN.md` §6.1 already names this exact risk: "Hosted multi-tenant:
  a host-level compromise... would otherwise expose every tenant's view key
  at once," and v1 ships only `PlainKeyCustody` (plaintext, in-process, no
  isolation). That was an acceptable default when every deployment was
  single-tenant and self-hosted (attacker who compromises the box already
  owns the one wallet on it regardless). Once this plan puts *many unrelated
  merchants'* view keys in one process, that tradeoff changes shape — worth
  a real decision (accept it for a small beta with strong host hardening and
  revisit before scaling up, vs. prioritizing a TEE-backed `KeyCustody`
  sooner) rather than an accidental default.
- Backups of the engine's SQLite file (it holds every tenant's sealed key
  material and full order history) with a tested restore procedure, not just
  a cron job nobody's verified.
- Basic incident runbook: what happens, and what we tell merchants, if the
  box is compromised — losing a view key is a privacy incident (who paid
  whom, how much, when), never a funds-loss one, per the engine's core
  design guarantee, and that distinction should be in the runbook explicitly
  since it changes the severity and disclosure conversation.

### Stage 10 — Legal/compliance (parallel track, start early)

Running a hosted service that touches merchants' payment flows, even
non-custodially (no spend key ever exists, per the engine's core design
guarantee), is worth a real look at money-transmission/licensing exposure in
whatever jurisdiction(s) this launches from, before public marketing rather
than after. Flagging this as a workstream to start now, in parallel with
engineering — not a blocker on Stage 1–8's code, but a blocker on Stage 7's
*public* distribution step specifically.

### Stage 11 — Shopify readiness check (not building yet)

Once Stages 1–7 are live, worth a short exercise confirming the split in §1
held up in practice: does adding a `"shopify"` platform to
`store_connections` and a Shopify-specific connect adapter actually require
zero engine changes and zero control-plane schema changes? If yes, the
architecture did its job. The follow-on work at that point is applying for
Shopify Partner + Payments App certification and building an Offsite Payment
Extension around the same `/pay/v1/{pk}/{payment_id}` redirect target the
WooCommerce plugin already uses — not revisiting anything built here.

## 4. Open questions for you

1. **Control-plane stack**: recommending Rust/axum for consistency with the
   engine (§Stage 2) — any reason to prefer something else (e.g. a framework
   with more off-the-shelf auth/dashboard scaffolding, at the cost of a
   second language in the codebase)?
2. **Where does the control plane live relative to this repo?** Same repo as
   a new crate/workspace member, or a separate repository entirely? Doesn't
   block the plan, but affects how Stage 2 actually starts.
3. **Beta scope**: launch Stage 1–7 to a small private list first, or aim
   straight for a public wordpress.org listing? Affects how hard Stage 9/10
   need to be finished before "done."

## 5. What I'd build first if you say go

Stage 1 (deploy the existing engine, hosted, admin API network-isolated) and
the first half of Stage 2 (the `users` table, signup/login, and a bare
`POST /connect/woocommerce/start` + `/finish` pair against a stubbed wallet
form) — that's the minimum slice that proves the "connect" mechanism end to
end before any WooCommerce-specific code exists, and it's fully decoupled
from every open question above except #1.
