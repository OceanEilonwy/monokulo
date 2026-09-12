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
- **One repository for the whole business**, control plane included — the
  business is open source end to end, not just the payment engine. See §3.1
  for how that's structured mechanically.
- **Stay on axum** for both the engine and the new control plane, rather than
  introducing Rocket. See §3.2 for the reasoning — this was a live question,
  not a foregone conclusion.
- **Prove the WooCommerce integration protocol with a mock before writing any
  PHP.** A small in-repo "fake WooCommerce" exercises the connect flow, order
  creation, and webhook receipt exactly as a real plugin would, so the
  protocol gets full e2e coverage (including real stagenet payments) before
  any WordPress-specific code exists. See §5, Stage 5.
- **The hosted engine's admin API stays open and anonymous**, matching how it
  already behaves for self-hosters — advanced users can talk to the raw
  engine directly, with no account required, same as today. This reverses
  what an earlier draft of this document assumed (network-isolating the
  admin API behind the control plane); §4 below is the debate that led here,
  written up in full since you asked for the reasoning, not just the
  conclusion.

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
- **The control plane** is a new service ("MoneroPay Cloud" in this doc) that
  owns *accounts* and *store connections*. It talks to the engine's existing
  admin API (`POST /api/v1/admin/tenants`, webhook registration, etc.) as a
  normal HTTP client — it does not reach into the engine's database or add
  new engine concepts. Its core data model is deliberately generic:

  ```
  users(id, email, password_hash, created_at)
  store_connections(id, user_id, platform, site_url, tenant_public_key,
                     tenant_secret_token_encrypted, moneropay_endpoint, created_at)
  ```

  `platform` is `"woocommerce"` today. Adding Shopify later means adding
  `"shopify"` as a second value and a second adapter — not touching the
  engine, not touching the `users` table, not touching how tenants are
  created. The connect flow (§5, Stage 6) is written generically for exactly
  this reason (it's phrased as "a platform sends us a return URL and gets a
  tenant back," which is true of any platform).
- **Each platform gets its own thin adapter** — the WooCommerce plugin now, a
  Shopify app later — that talks to the engine's *public* API directly at
  runtime (order creation, checkout redirect, webhook receipt), and to the
  control plane only during the one-time connect flow. This mirrors
  `docs/DESIGN.md` §14's client-library design (thin edge, all real logic
  server-side), and is why the plugin's payment-processing core (§5, Stage 7)
  deliberately redirects to the engine's existing hosted checkout page rather
  than embedding a new custom UI in WooCommerce's checkout: it's the same
  shape a future Shopify Offsite Payment Extension would need (redirect the
  buyer to an app-hosted payment page), so building it once for WooCommerce
  is also a dry run for Shopify's certified extension model.

If you disagree with "separate control-plane service" as the split point,
that's the one architectural call in this doc I'd flag as worth a second look
before any of §5's stages start — everything downstream assumes it.

## 2. Current state vs. target

What already exists and needs no new engine code:

- Tenant creation, order lifecycle, payment matching, reorg/double-spend
  handling, webhook delivery — all implemented and tested (`src/scanner.rs`,
  `src/store.rs`, `src/webhook_delivery.rs`).
- `POST /api/v1/admin/tenants` already accepts a wallet's view key + public
  spend key and returns `{tenant_id, public_key, secret_token}` — this *is*
  the "create a hosted account" primitive; it just has no signup UI or email
  identity wrapped around it yet, and (per §0/§4) is staying anonymous rather
  than gaining one at the engine layer.
- The hosted-vs-self-hosted split is already a config-time decision (`[wallet]`
  present or absent), not a code fork.
- `/pay/v1/{pk}/{payment_id}` is already a complete, styled, working checkout
  page with QR code, live status, double-spend banner.
- Rate limiting (`governor`, per `docs/DESIGN.md` §12/§15) and the SSRF-safe
  webhook delivery worker already exist and need no new code to serve a
  hosted deployment — they were designed for exactly this.

What's missing, all of which this plan builds:

- No user accounts, no dashboard, no persistent notion of "this merchant" for
  anyone who *wants* that convenience layer.
- No WooCommerce integration of any kind, mocked or real.
- No live exchange-rate provider (only `"fixed"`, i.e. hand-entered rates).
- `PlainKeyCustody` (the only `KeyCustody` implementation) keeps every
  tenant's view key in plaintext in one process's memory — a real
  consideration once "hosted" means *many unrelated merchants* on the same
  box, flagged explicitly in `docs/DESIGN.md` §6.1 and revisited in §5,
  Stage 11.

## 3. Two decisions this update settles

### 3.1 Repository structure: a Cargo workspace, not a rewrite

"One repo for everything" is straightforward mechanically, and — importantly
— doesn't require moving or renaming any of the existing, tested engine code.
Cargo supports a manifest that is simultaneously a package *and* the
workspace root, so the existing root `Cargo.toml` gains a `[workspace]` table
alongside its current `[package]` table, and new sibling crates join as
members:

```
/Cargo.toml            # existing [package] (the engine) + new [workspace]
/src/...               # unchanged — the engine, exactly as it is today
/control-plane/        # new crate: accounts, store_connections, connect flow, dashboard backend
/shared/                # new crate: things both sides want (argon2 config, secret-token
                        #   generation/hashing from src/auth.rs, HMAC helpers from
                        #   src/webhook_sign.rs) — pulled out so the control plane isn't
                        #   duplicating logic the engine already got right
/mock-woocommerce/      # new crate: the fake WooCommerce from §5, Stage 5
/plugins/woocommerce/   # the real PHP plugin (§5, Stage 7) — not a Cargo crate, just
                        #   lives in the same repo since "the whole business is open source"
/docs/
```

No file in the existing engine tree moves. `cargo build`/`cargo test` from
the repo root builds/tests everything by default once the workspace table
exists; CI, the e2e stagenet harness, and existing paths all keep working
unchanged. This is the lowest-disruption way to get to "one repo, one
business" — worth calling out explicitly since the alternative (moving the
existing engine into `engine/src/...` to make room) would touch every file
path for zero functional gain.

### 3.2 Framework: staying on axum, for both services

You asked me to weigh Rocket against axum, conditional on Rocket actually
reducing code for OAuth/rate-limiting/logging and simplifying the existing
engine. Having thought it through:

- **OAuth isn't a framework feature in Rust, in either framework.** Neither
  axum nor Rocket ships an OAuth *provider* (issuing tokens to third parties)
  or *client* (logging a user in via Google/GitHub) out of the box. Both
  approaches end up using the same framework-agnostic `oauth2` crate and
  wiring the redirect/callback handlers by hand — the amount of code is
  essentially identical either way. This is different from, say, Django or
  Rails, which do have batteries-included OAuth provider packages; no
  mainstream Rust web framework is at that level, so switching frameworks
  buys nothing here.
- **Rate limiting**: the engine already uses `governor` (`docs/DESIGN.md`
  §15, implemented in `src/http/rate_limit.rs`), which is framework-agnostic
  at its core — the axum integration is already done and tested. Using it
  from Rocket would mean writing a Fairing to call the same `governor`
  primitives by hand; axum's `tower`-based middleware ("layers") is if
  anything the more mature integration point for this specific crate today.
  Net code difference: roughly zero, mildly in axum's favor because it's
  already built.
- **Logging**: comparable either way (both integrate fine with `tracing`);
  no meaningful win for either framework.
- **"Simplify the existing engine"**: this would mean rewriting
  `src/http/*.rs` (currently ~1,150 lines of route handlers plus 992 lines of
  tests, all working and audited-by-use) onto a different framework, for a
  capability the engine doesn't need (it isn't adding OAuth — its own
  bearer-token scheme in `src/auth.rs` is a deliberately simpler, bespoke
  fit for a machine-to-machine payment API, and a generic OAuth *server*
  library wouldn't replace it, just sit next to it in the control plane). I
  can't find a version of this that isn't "large rewrite, real regression
  risk, no new capability" — recommend leaving the engine exactly as it is.
- **For the new control plane**: since it's green-field, the choice is much
  lower-stakes either way, and I'd lean axum there too mainly for
  *consistency* — one HTTP stack across the repo, the `shared` crate's
  helpers (§3.1) are directly reusable without an adapter layer, and anyone
  who's worked on the engine can read the control plane without context-
  switching frameworks. If there's a specific axum pain point that prompted
  considering Rocket — something concrete you hit or read about — I'd like
  to hear it, since that might change this recommendation; absent that, I
  don't see a case for a second framework.

## 4. Debate: should the engine's admin API require an account?

You asked me to argue this out rather than just pick, because the goal you
stated — "our public SaaS would enable advanced users to use the underlying
core API anonymously" — is in real tension with the control plane's reason
to exist (accounts, billing, abuse accountability). Three positions, honestly
weighed:

**Option A — Lock the admin API behind the control plane entirely.**
Network-isolate `/api/v1/admin/*` so only the control plane can reach it;
every tenant on the hosted instance is necessarily account-linked.

- *For*: every tenant traceable to an email for support/abuse/incident
  response; a coherent, enforceable base for any future paid tier (unlimited
  free anonymous tenant creation and metered billing cannot coexist — this
  option removes that tension by construction); simplest mental model
  ("hosted tenants always have an owner").
- *Against*: directly contradicts the stated goal. It also throws away
  something that already works — the admin API *is* a complete, tested
  self-serve provisioning primitive today, and hiding it duplicates that
  functionality behind a second, bespoke signup path for no functional gain
  to anyone who doesn't want a dashboard.

**Option B — Leave the admin API exactly as it is: open, anonymous, DDoS-layer
rate-limited only.** The control plane becomes a pure convenience layer on
top, calling the same public endpoint any anonymous user could call.

- *For*: literally zero new engine code — this is the status quo, and it
  satisfies "advanced users can use the core API anonymously" as directly as
  possible, since it's already true today for self-hosters and would stay
  true for the hosted instance too. Keeps the engine's own design promise
  intact: it doesn't know or care whether a caller came from a dashboard, a
  curl command, or a plugin.
- *Against*: we're now the ones paying for the node, the disk, and the
  scanning CPU that anonymous tenants consume, unlike the self-hosted case
  where that cost sits with whoever's already choosing to pay it. The
  existing DDoS-layer defenses (per-IP token bucket) bound *request rate*,
  not *account count* — nothing stops a moderately distributed script from
  creating many tenants over time at a trickle each defense doesn't notice.
  Any future paid tier is unenforceable against this path by construction
  (§0 already commits to accepting this — worth being explicit that it's a
  real, permanent limitation of the "anonymous" promise, not a gap to later
  close). It also means a tenant can exist that the control plane has never
  heard of, which Stage 6's connect flow needs to handle gracefully (see
  "adopt an existing tenant" below) rather than assuming it minted every
  tenant it knows about.

**Option C — Keep the admin API open and anonymous (Option B's core
position), but tune the existing rate-limiting/PoW knobs specifically for the
hosted deployment, and let the control plane *adopt* an existing tenant as a
first-class alternative to creating a new one.**

- The engine already has the knobs for this without any new code:
  `rate_limit_per_ip_per_min` is already configurable per-deployment, and
  `docs/DESIGN.md` §12 already scopes an "optional JS proof-of-work
  challenge, gated behind a load threshold" as a defense specifically meant
  to stay invisible to normal traffic and only engage under load — turning
  that on for the hosted instance's admin endpoint raises the cost of
  anonymous abuse without removing the capability anonymous users are
  supposed to keep.
- "Adopt an existing tenant" means the connect flow (§5, Stage 6) gains a
  second entry point alongside "create a new tenant": paste an existing
  `pk_`/`sk_`, the control plane calls `GET /api/v1/admin/tenant` with that
  `sk_` to confirm it's real and fetch its details, and creates a
  `store_connections` row pointing at it — no new tenant minted. This is a
  small addition (one more branch in one flow), and it's what makes Option
  C actually coherent: someone who started anonymously (curl, or a
  self-hosted instance they're migrating off of) can later bolt on
  WooCommerce or the dashboard without migrating anything.
- *For*: gets both things you asked for — genuine, permanent anonymous power-
  user access, and a real accounts/billing story for anyone who wants it —
  using mechanisms the engine already has rather than inventing new ones.
- *Against*: more surface area than B (the adopt-flow), and abuse is
  bounded, not eliminated — this is inherent to the feature, not a flaw in
  this particular design. It also means "how many free anonymous tenants is
  acceptable" becomes a real operational number someone has to pick and
  watch (§5, Stage 11 already has a monitoring stage this folds into), not
  a one-time decision.

**My read**: Option C is the only one of the three that actually honors the
stated goal while giving the business something to build a future paid tier
on for the people who *do* want an account — Option A quietly drops the
goal, Option B is the goal taken completely literally with no throttle at
all. But C's central bet — that tuned rate limiting plus an optional PoW
challenge is enough friction to keep hosting costs sane — is a real
prediction about abuser behavior, not a proof, and it's worth you weighing in
on directly rather than me picking it unilaterally given you asked for a
debate specifically. If C is right, the concrete follow-up decision is just
picking starting numbers (rate limit ceiling, whether PoW is on from day
one or held in reserve for when load actually shows abuse) — I'd default to
launching with today's existing defaults unchanged and watching real usage
before tightening anything, so as not to add friction that turns out to be
unnecessary.

This also *simplifies* Stage 1 below relative to the previous draft of this
document, which had proposed network-isolating the admin API — that's no
longer needed under B or C, only under A.

## 5. Ordered plan

### Stage 1 — Stand up the hosted engine

Deploy `moneropay-core` itself, unmodified, as a running service:

- Provision the box (or VM), point `[monero_node]` at the chosen curated
  node, run `moneropay-core --init` once to produce the config (no `[wallet]`
  section — hosted mode creates tenants at runtime), start it under a
  process supervisor (systemd unit is enough at this scale).
- TLS termination and the public domain (`https://pay.<yourdomain>` or
  similar — used as `moneropay_endpoint` in the control plane's data model
  in §1).
- The admin API stays public, per §4 — no network-isolation work needed here
  under Option B or C. If C is the direction, this stage is where
  `rate_limit_per_ip_per_min` gets set deliberately for the hosted case
  (rather than inherited blindly from the self-hosted default) and where the
  PoW challenge config gets decided (on from day one vs. held in reserve).
- Basic monitoring: process up/down, node sync height, disk space (the SQLite
  file), webhook delivery queue depth, and (per §4) anonymous-vs-account-
  linked tenant creation volume, so "is Option C's bet holding up" is an
  answerable question rather than a guess.

This stage is almost entirely ops work, not new code — the payoff of the
engine already having been designed for multi-tenancy.

### Stage 2 — Workspace restructuring

Mechanical, small, and worth doing before any control-plane code lands: add
the `[workspace]` table to the root `Cargo.toml` and create the `shared/`,
`control-plane/`, and `mock-woocommerce/` crate skeletons (empty `lib.rs`/
`main.rs`, just enough to `cargo build` the workspace) per §3.1. Pull the
genuinely-reusable pieces of `src/auth.rs` (secret-token generation/hashing)
and `src/webhook_sign.rs` (HMAC signing/verification) into `shared/` at this
point too, with the engine updated to depend on `shared` for them rather than
keeping its own copy — so there's exactly one implementation of "how we hash
a bearer secret" and "how we sign/verify a webhook" in the repo, not one per
service.

### Stage 3 — Control-plane service: accounts + store connections

New crate (`control-plane/`), new small database (schema in §1). This is the
first stage that's genuinely new code rather than restructuring:

- **Signup/login**: email + password, `argon2` via the `shared` crate's
  helper (Stage 2) rather than a second implementation, sessions via a
  server-side session table (easier to force-revoke than a bare signed
  cookie once "delete my account" or "log out everywhere" exist).
- **Store connection abstraction**: a generic "start a connect flow for
  platform X, site Y" endpoint and a generic "finish it and hand back
  credentials" endpoint — fleshed out fully in Stage 6, but the *shape*
  should exist before any WooCommerce-specific code is written, precisely
  so Shopify can reuse it later.
- **Tenant provisioning, two paths** (per §4's Option C):
  - *Create*: takes the wallet fields a merchant enters (primary address,
    private view key, public spend key, network — the same ones the CLI
    wizard already collects), calls the engine's `POST
    /api/v1/admin/tenants`, stores the resulting `pk_`/`sk_` against a new
    `store_connections` row.
  - *Adopt*: takes an existing `pk_`/`sk_` pair, calls `GET
    /api/v1/admin/tenant` with that `sk_` to validate it and fetch its
    details, stores a `store_connections` row pointing at it without
    minting anything new.
  - Either way, `sk_` at rest is encrypted with a key the control plane
    holds (a KMS key or an environment-provided secret, never committed) —
    used server-to-server only (webhook registration, future account-
    management features), never re-shown to the merchant after the initial
    connect flow.

### Stage 4 — Dashboard UI

The web surface a merchant actually sees:

- Signup/login pages.
- "Connect a store" — for the MVP, effectively one button ("Connect
  WooCommerce"), but written as a list so a second platform is an entry, not
  a redesign. Includes the "I already have a `pk_`/`sk_`" adopt-path from
  Stage 3 as a secondary option on the same screen, for advanced users who
  provisioned anonymously and want to add WooCommerce/dashboard convenience
  after the fact.
- A wallet-connection form (address + view key + public spend key,
  currency/threshold defaults) for the create-path — this is the one
  inherently non-custodial step that a competitor holding customer funds
  doesn't have, and it's worth keeping the copy on this page honest about
  *why* it's needed (never pretend it can go away).
- Order list and detail (wrapping `GET /api/v1/admin/tenant/orders` and the
  detail route) and webhook management (list/rotate) — this is the GUI that
  replaces `local_admin.rs`'s CLI flags (`--show-tenant`, `--rotate-secret`)
  for anyone who isn't comfortable on a terminal, which was one of the
  original gaps identified versus Shop Pay.

### Stage 5 — Mock WooCommerce: prove the protocol before writing PHP

Per your third answer: before touching WordPress at all, build a small
in-repo stand-in (`mock-woocommerce/`, an axum service like anything else in
this workspace) that interacts with the engine and control plane exactly the
way a real WooCommerce plugin will, so the whole connect → pay → webhook
protocol gets proven end-to-end in Rust, with fast iteration and full test
control, before any PHP/WordPress-specific risk is introduced. Concretely it
implements:

- A fake "merchant site" HTTP endpoint standing in for the plugin's settings
  page — receives the connect-flow redirect (Stage 6 below) the same way the
  real plugin's `return_url` would, and makes the same server-to-server
  "finish" call to exchange the connect token for real credentials.
- A driver that calls `POST /api/v1/t/{pk}/orders` the same way
  `process_payment()` will, and follows the same redirect-to-checkout-page
  pattern (or just asserts the redirect target is well-formed, since there's
  no real browser here).
- A fake webhook receiver implementing the same `X-MoneroPay-Signature`
  verification and `event_id` dedupe logic the real plugin needs (Stage 8),
  recording what it received so tests can assert on it.

Then: a true end-to-end test wires this mock together with the engine and
control plane, using the engine's existing stagenet e2e infrastructure
(`e2e/stagenet-wallets.json`, patterns from `e2e/demo-shop/`) to push a real
stagenet payment through the entire flow — signup, connect, order creation,
real on-chain payment, webhook delivery and verification — and asserts the
mock ends up in the right final state. This is the gate before Stage 7/8:
once this passes reliably, the *protocol* is proven, and writing the real
PHP plugin becomes a faithful port of already-working logic rather than
simultaneously debugging WordPress and the protocol.

### Stage 6 — Connect flow (implemented once, exercised by the mock first)

This is the actual answer to "OAuth-style one-click install," and it's worth
being precise about what it is *not*: it is **not** WooCommerce's own
`wc-auth/v1/authorize` mechanism. That endpoint grants a *third party* access
to a store's own WooCommerce REST API (orders/products) — the direction
you'd need if MoneroPay wanted to read WooCommerce's data remotely. We don't:
the plugin runs *inside* WordPress and already has full native access to
everything it needs (order objects, hooks) with no REST API keys involved.
The actual mechanism, closer to how Stripe's or WooCommerce Payments' own
"Connect" buttons work, and written generically enough that Stage 5's mock
exercises the identical flow a real plugin will:

1. The (mock, then real) plugin generates and stores a nonce locally, then
   redirects to
   `https://cloud.moneropay.example/connect/woocommerce?site_url=<store>&return_url=<settings page>&nonce=<random>`.
2. On our dashboard: sign up or log in if not already (Stage 3/4), or, for
   the adopt-path, paste an existing `pk_`/`sk_` directly.
3. Dashboard shows "Connect Monero payments for `<site_url>`" and either the
   wallet form (create) or the paste-your-keys form (adopt) from Stage 4. On
   submit, the control plane provisions or validates the tenant (Stage 3),
   registers a webhook (`POST /api/v1/admin/tenant/webhooks`, pointed at the
   URL the plugin defines — Stage 8) using the `sk_`, and stores the
   `store_connections` row.
4. Redirects back to `return_url` carrying a short-lived, single-use, signed
   connect token — **not** the raw `sk_` — to avoid a secret ever sitting in
   a browser history, a referrer header, or an access log.
5. The (mock, then real) plugin, on receiving that redirect, makes a
   *server-to-server* call (not the browser) to
   `POST https://cloud.moneropay.example/connect/woocommerce/finish` with
   the token, gets back the real `pk_`/`sk_`/`endpoint`/webhook signing
   secret in the response body, verifies the stored nonce matches, and saves
   everything.

### Stage 7 — Real WooCommerce plugin: payment-processing core

Once Stage 5's e2e test is green, port the mock's order-creation logic into
an actual WordPress plugin:

- `class WC_Gateway_MoneroPay extends WC_Payment_Gateway`, registered the
  normal WooCommerce way (`woocommerce_payment_gateways` filter).
- **Integration style: redirect to the engine's existing hosted checkout
  page, not a custom in-checkout widget.** `process_payment( $order_id )`
  calls `POST /api/v1/t/{pk}/orders` (server-side, from PHP — no browser
  CORS/`allowed_origins` concern here, since the call never leaves the
  merchant's server) with the WooCommerce order's total and currency, then
  returns WooCommerce's normal "redirect the customer to this URL" result
  pointing at `/pay/v1/{pk}/{payment_id}`. This is deliberately the simplest
  possible integration (the same pattern as classic PayPal Standard-style
  gateways), reuses 100% of the already-built and already-tested checkout
  page, and needs zero new customer-facing UI. It's also, not coincidentally,
  the same shape a Shopify Offsite Payment Extension needs (§1).
- `merchant_order_id` on the created order is the WooCommerce order ID, so
  the webhook payload (Stage 9) can map back to a WooCommerce order without
  a separate lookup table.
- Store `pk_`/`sk_`/`endpoint` in WooCommerce's own gateway settings
  (`WC_Payment_Gateway`'s standard settings storage, i.e. `wp_options`) — the
  same trust boundary every other WooCommerce payment gateway plugin already
  uses for its API keys (Stripe's secret key lives the same way); no new
  security model needed here.

### Stage 8 — Real WooCommerce plugin: connect flow

Port Stage 6's flow, already proven against the mock, into the plugin's
settings screen: the "Connect your Monero wallet" button, the redirect, and
the server-to-server "finish" call (PHP's `wp_remote_post`), landing on the
same control-plane endpoints Stage 5's mock already validated.

End-to-end merchant-visible steps once this and Stage 7 are done: install
plugin → click Connect → sign up/log in on our site (or paste existing keys)
→ done. That's the ceiling on "one click" that a genuinely non-custodial
system can offer — the one step a custodial competitor skips (entering your
own wallet, for the create-path) is real and should stay visible, not
disguised.

### Stage 9 — Real WooCommerce plugin: order status sync

Port Stage 5's mock webhook receiver into the plugin, for real this time:

- A normal WordPress URL via WooCommerce's `woocommerce_api_{$this->id}`
  hook (e.g. `https://theirsite.com/wc-api/moneropay_webhook`) — no
  WooCommerce REST API keys needed, just a plain endpoint the plugin owns,
  registered as the webhook URL back in Stage 6 step 3.
- Verify `X-MoneroPay-Signature` (HMAC-SHA256, per `docs/DESIGN.md` §11),
  map event → WooCommerce order status: `order.paid`/`order.overpaid` →
  `processing` (or `completed`, merchant's choice), `order.expired` →
  `cancelled`, `order.double_spend_detected` → an order note + hold for
  manual review (never auto-cancel on this alone — it's informational per
  the engine's own design, §7.5/§7.6).
- Dedupe on `event_id` (order meta) — at-least-once delivery, so the handler
  must be idempotent regardless of how reliable delivery turns out in
  practice.
- Final verification pass: a real WordPress/WooCommerce dev environment
  (`wp-env`) alongside the real engine on stagenet, confirming the ported PHP
  behaves identically to what Stage 5's mock already proved — this should be
  a much smaller effort than building the protocol from scratch would have
  been, since by this point the *logic* is already correct and this is
  mostly "does WordPress's specific plumbing work."

### Stage 10 — Distribution

- Publish the plugin to the wordpress.org plugin directory. Free, no
  partner-approval gate (unlike Shopify) beyond directory guidelines — it's
  what makes "search WooCommerce Marketplace/wp-admin, click Install"
  possible at all.
- A direct deep link from our own marketing site
  (`wp-admin/plugin-install.php?tab=plugin-information&plugin=<slug>`) that,
  for a merchant already logged into their own wp-admin, lands directly on
  the plugin's "Install Now" button — the closest thing to a true one-click
  link WordPress's own model allows for a third party.

### Stage 11 — Exchange-rate automation (parallel track, not blocking)

`docs/DESIGN.md` §13 already sketches pluggable providers
(`haveno | kraken | coingecko | fixed`); only `"fixed"` exists today. A
WooCommerce store commonly needs several currencies without a human typing
in a rate — implement at least one live provider (coingecko is the simplest
to start with) before or shortly after Stage 10's public launch. Doesn't
block the plugin working — a hosted instance could launch with a couple of
hand-maintained rates and swap the provider under it later with zero plugin
changes, since the plugin never sees exchange rates directly.

### Stage 12 — Hosted-specific hardening (gate before public launch, not before private beta)

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
  material and full order history) with a tested restore procedure, not
  just a cron job nobody's verified.
- If §4's Option C is the direction taken: confirm the anonymous-tenant
  monitoring from Stage 1 is actually in place and being watched, not just
  planned.
- Basic incident runbook: what happens, and what we tell merchants, if the
  box is compromised — losing a view key is a privacy incident (who paid
  whom, how much, when), never a funds-loss one, per the engine's core
  design guarantee, and that distinction should be in the runbook explicitly
  since it changes the severity and disclosure conversation.

### Stage 13 — Legal/compliance (parallel track, start early)

Running a hosted service that touches merchants' payment flows, even
non-custodially (no spend key ever exists, per the engine's core design
guarantee), is worth a real look at money-transmission/licensing exposure in
whatever jurisdiction(s) this launches from, before public marketing rather
than after. Flagging this as a workstream to start now, in parallel with
engineering — not a blocker on Stages 1–11's code, but a blocker on Stage
10's *public* distribution step specifically.

### Stage 14 — Shopify readiness check (not building yet)

Once Stages 1–10 are live, worth a short exercise confirming the split in §1
held up in practice: does adding a `"shopify"` platform to
`store_connections` and a Shopify-specific connect adapter actually require
zero engine changes and zero control-plane schema changes? If yes, the
architecture did its job. The follow-on work at that point is applying for
Shopify Partner + Payments App certification and building an Offsite Payment
Extension around the same `/pay/v1/{pk}/{payment_id}` redirect target the
WooCommerce plugin already uses, and — per §4/§5 Stage 5's approach — the
same "prove it with a mock first" method should apply there too, before any
Shopify-side app code is written.

## 6. Open questions for you

1. **§4's debate**: which of Option A/B/C do you want to run with? I'm
   leaning C, but flagged it as a real prediction rather than a settled
   answer on purpose.
2. **Axum vs. Rocket**: was there something specific about axum that
   prompted considering Rocket — a concrete pain point, not just "is there a
   framework that makes this easier" — that I should know about before
   settling on "stay on axum" for good?
3. **Anonymous-tier starting numbers**, if Option C: keep
   `rate_limit_per_ip_per_min` at today's self-hosted default for launch and
   only tighten it (or turn on the PoW challenge) if real abuse shows up, or
   start more conservative on day one? I'd default to the former (don't add
   friction pre-emptively) unless you see a reason to expect abuse
   immediately.
4. **Beta scope**: launch Stages 1–10 to a small private list first, or aim
   straight for a public wordpress.org listing? Affects how hard Stage
   12/13 need to be finished before "done."
5. **Monetization, at a high level, even if not now**: is a future paid tier
   actually planned (which makes §4's tension real and worth designing
   around now), or is anonymous-and-free the permanent model (in which case
   the control plane's job is purely convenience/UX, never billing, and some
   of Stage 3's "future account-management features" framing can simplify)?

## 7. What I'd build first if you say go

Stage 2 (workspace restructuring — small, mechanical, unblocks everything
else) followed by Stage 3's `create` path and the first half of Stage 6 (a
bare `POST /connect/woocommerce/start` + `/finish` pair against a stubbed
wallet form, no real dashboard yet) — that proves the connect mechanism
end to end before any WooCommerce-specific code, mock or real, exists, and
it's fully decoupled from every open question above except #1 and #2.
