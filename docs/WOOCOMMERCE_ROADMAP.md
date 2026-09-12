# MoneroPay Cloud — WooCommerce MVP Roadmap

Status: planning document, no code yet. This lays out an ordered path from the
current self-hosted `moneropay-core` CLI tool to a hosted service that a
merchant can sign up for and get a working "Pay with Monero" option in their
WooCommerce checkout in a handful of clicks.

For the task-by-task execution breakdown (small, sequenced, individually
testable chunks), see `docs/WOOCOMMERCE_WBS.md`.

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
- **The hosted engine's admin API is locked behind the control plane**
  (§4's Option A) — every tenant on the hosted instance is account-linked,
  full stop. §4 below is kept as the debate that led here, written up in
  full since you asked for the reasoning, not just the conclusion, plus the
  resolution and what it changes downstream.
- **A control-plane-layer anonymous "pay by Monero" API is parked, not
  built now.** The idea — advanced users get an anonymous account *at the
  control-plane layer itself* (not direct anonymous access to the engine's
  admin API) — is a real future feature, sketched briefly in §6, but out of
  scope for this MVP. Nothing in this plan should be built in a way that
  makes it hard to add later, but nothing here builds it either.
- **Stay on axum — confirmed, no open pain point.** §3.2's recommendation
  stands with no caveats.
- **TEE-backed `KeyCustody` is a hard gate before *any* go-live**, including
  the initial private beta — not a "revisit later" risk acceptance. See §5,
  Stage 12.
- **AMD SEV-SNP, not TDX or AWS Nitro Enclaves**, and built as a confidential
  VM plus a separate minimal `key-custody-service` process rather than a
  full enclave-split rewrite. See Stage 12 for the reasoning — mainly
  SEV-SNP's VMPL feature fitting the process-split design, both now being
  available across all three major clouds (removing the vendor-lock-in
  concern that ruled out Nitro), and a 2025 SEV-SNP vulnerability
  (StackWarp) that's since been patched by AMD.
- **Launch to a small private group first.** Legal/compliance work (§5,
  Stage 14) explicitly does not need to be finished before that — it's a
  blocker on going public, not on the private beta.
- **Not-for-profit.** The service doesn't charge for real (email-linked)
  accounts. The only place money changes hands is the parked anonymous-
  access feature (§6), and even there the fee's purpose is deterring abuse,
  not generating revenue.

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
  the "create a hosted account" primitive; the control plane calls it
  server-to-server rather than replacing it, and (per §0/§4) it stops being
  directly reachable from outside once the hosted instance is network-
  isolated.
- The hosted-vs-self-hosted split is already a config-time decision (`[wallet]`
  present or absent), not a code fork.
- `/pay/v1/{pk}/{payment_id}` is already a complete, styled, working checkout
  page with QR code, live status, double-spend banner.
- Rate limiting and the SSRF-safe webhook delivery worker already exist and
  need no new code to serve a hosted deployment — they were designed for
  exactly this. Correction from an earlier draft, found on rereading the
  actual code rather than `docs/DESIGN.md` §15's dependency table: the
  limiter is a hand-rolled fixed-window per-IP counter
  (`src/http/rate_limit.rs`), not the `governor` crate that table lists —
  the table appears to be aspirational/stale rather than a description of
  what's actually implemented. Doesn't change anything downstream (the
  limiter is real, tested, and configurable either way), but worth not
  repeating the wrong dependency name.

What's missing, all of which this plan builds:

- No user accounts, no dashboard, no persistent notion of "this merchant" for
  anyone who *wants* that convenience layer.
- No WooCommerce integration of any kind, mocked or real.
- No live exchange-rate provider (only `"fixed"`, i.e. hand-entered rates).
- `PlainKeyCustody` (the only `KeyCustody` implementation) keeps every
  tenant's view key in plaintext in one process's memory — a real
  consideration once "hosted" means *many unrelated merchants* on the same
  box, flagged explicitly in `docs/DESIGN.md` §6.1 and — per your answer,
  now a hard go-live gate rather than a deferred concern — addressed head-on
  in §5, Stage 12.

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
/shared/                # new crate: logic pulled out of the engine so the control
                        #   plane isn't duplicating it (secret-token generation/hashing
                        #   from src/auth.rs, HMAC helpers from src/webhook_sign.rs, the
                        #   migration runner from src/store.rs — see §5, Stage 2), plus
                        #   a fresh argon2 password-hashing helper for real user
                        #   accounts, which is genuinely new (the engine hashes its own
                        #   sk_ tokens with plain SHA-256 on purpose — see Stage 3)
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
- **Rate limiting**: the engine already has a working, tested per-IP limiter
  as `axum` middleware (`src/http/rate_limit.rs` — a hand-rolled fixed-window
  counter, not the `governor` crate `docs/DESIGN.md` §15's table names; that
  table looks stale against the real dependency list). Whichever crate ends
  up backing this, it's already integrated and tested against axum; moving
  to Rocket would mean re-doing that integration as a Fairing for no
  behavioral gain.
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
  watch, not a one-time decision.

**Resolution: Option A.** Every tenant on the hosted instance is
account-linked; the admin API is network-isolated behind the control plane
exactly as this section originally proposed. The reasoning: Option A is the
only one of the three with a clean, permanent answer to "who does this
tenant belong to," which is worth more than the marginal convenience of
letting the *raw engine* stay anonymously reachable — especially once
there's a real business (accounts, support, eventually billing) sitting on
top of it. It also has a nice, unplanned side effect on the rest of this
document: the "adopt an existing tenant" branch that Option C required
(§5, old Stage 3/6) disappears — under Option A no tenant can exist on the
hosted instance without the control plane having created it, so there's
only ever one provisioning path, not two. That's real code that doesn't
need to be written; see the Stage 3/4/6 edits below.

The part of the original goal this drops — a *fully* anonymous path to the
raw engine — isn't abandoned, just relocated: §6 sketches an anonymous
account *at the control-plane layer* as a parked future feature, which gets
the "no email, no signup friction" property back without reopening the
"anyone can create unlimited tenants on infrastructure we pay for, with no
way to ever attach billing to it" problem Option B/C's *For* cases couldn't
avoid. That's a deliberate choice to solve "anonymous" and "accountable" as
two separable concerns rather than one axis with no good midpoint.

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
- **Network-isolate the admin API**, per §4's Option A. Today `POST
  /api/v1/admin/tenants` (and the rest of `/api/v1/admin/*`) is
  intentionally open on a self-hosted deployment (DDoS-layer-only, per
  `docs/DESIGN.md` §10.1/§12) — correct there, since "the operator" and "the
  person hitting the API" are the same trusted party. On the hosted instance
  that's no longer true, so bind `/api/v1/admin/*` to a private interface
  (or a reverse-proxy rule) reachable only from the control plane's network,
  while `/api/v1/t/{pk}/...` and `/pay/v1/...` stay public exactly as
  designed. No engine code change — purely a deployment-topology decision.
- Basic monitoring: process up/down, node sync height, disk space (the SQLite
  file), webhook delivery queue depth.

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

Two more moves belong in this same stage, found on rereading the actual code
rather than assuming from `docs/DESIGN.md`: `store.rs` already has a small,
generic, well-tested migration runner (`apply_migration_list`, transactional,
tracked in a `schema_migrations` table, works against any `&[(i64, &str)]`
list of SQL) that the control plane's own new database should reuse rather
than pulling in a different migration framework — move it into `shared`
alongside the other two. And since `mock-woocommerce` (Stage 5) and the
control plane's own tests both need to stand up a real, network-reachable
engine instance for integration tests, add a small test-harness helper now
(in `shared`, or a dev-only sibling crate) that does what
`tests/e2e_stagenet.rs` already does — build the router via
`moneropay_core::http::build_router`, bind it to `127.0.0.1:0` in a
background task, hand back the real address — rather than each later stage
reinventing it, or worse, shelling out to the compiled binary as a
subprocess. This also means `mock-woocommerce`'s `Cargo.toml` needs
`moneropay-core` itself as a (dev-)dependency, which is worth setting up here
rather than discovering it's missing three stages later.

### Stage 3 — Control-plane service: accounts + store connections

New crate (`control-plane/`), new small database (schema in §1). This is the
first stage that's genuinely new code rather than restructuring:

- **Signup/login**: email + password, hashed with `argon2` via a `shared`
  crate helper. Worth being precise about what's reused and what's new here:
  the engine already hashes *its own* `sk_` tokens, but deliberately with
  plain SHA-256, not `argon2` — correct for that case (`src/auth.rs`'s own
  doc comment: a machine-generated, high-entropy token gets no
  brute-force-resistance benefit from a slow hash, only the cost of one).
  A human-chosen password is the opposite case and genuinely needs `argon2`
  or similar — this is a new dependency and a new helper for `shared`, not a
  reuse of anything that already exists. Sessions via a server-side session
  table (easier to force-revoke than a bare signed cookie once "delete my
  account" or "log out everywhere" exist).
- **Store connection abstraction**: a generic "start a connect flow for
  platform X, site Y" endpoint and a generic "finish it and hand back
  credentials" endpoint — fleshed out fully in Stage 6, but the *shape*
  should exist before any WooCommerce-specific code is written, precisely
  so Shopify can reuse it later.
- **Tenant provisioning**: one path, per §4's Option A resolution — takes
  the wallet fields a merchant enters (primary address, private view key,
  public spend key, network — the same ones the CLI wizard already
  collects), calls the engine's `POST /api/v1/admin/tenants`, stores the
  resulting `pk_`/`sk_` against a new `store_connections` row. There's no
  "adopt an existing tenant" branch to build: under Option A no tenant can
  exist on the hosted instance the control plane didn't create, so this is
  the only provisioning path, not one of two.
  `sk_` at rest is encrypted with a key the control plane holds (a KMS key
  or an environment-provided secret, never committed) — used server-to-
  server only (webhook registration, future account-management features),
  never re-shown to the merchant after the initial connect flow.

### Stage 4 — Dashboard UI

The web surface a merchant actually sees:

- Signup/login pages.
- "Connect a store" — for the MVP, effectively one button ("Connect
  WooCommerce"), but written as a list so a second platform is an entry, not
  a redesign.
- A wallet-connection form (address + view key + public spend key,
  currency/threshold defaults) — this is the one inherently non-custodial
  step that a competitor holding customer funds doesn't have, and it's
  worth keeping the copy on this page honest about *why* it's needed (never
  pretend it can go away).
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
2. On our dashboard: sign up or log in if not already (Stage 3/4).
3. Dashboard shows "Connect Monero payments for `<site_url>`" and the wallet
   form from Stage 4. On submit, the control plane provisions the tenant
   (Stage 3), registers a webhook (`POST /api/v1/admin/tenant/webhooks`,
   pointed at the URL the plugin defines — Stage 8) using the `sk_`, and
   stores the `store_connections` row.
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
plugin → click Connect → sign up/log in on our site → paste wallet keys once
→ done. That's the ceiling on "one click" that a genuinely non-custodial
system can offer — the one step a custodial competitor skips (entering your
own wallet) is real and should stay visible, not disguised.

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

### Stage 12 — TEE-backed `KeyCustody` (hard go-live gate)

Per your answer, this is no longer the "accept the risk for a small beta,
revisit later" framing the previous draft had — it's a requirement before
*any* real user, including the initial private group, is onboarded. Worth
treating as its own real engineering track, not a bullet inside general
hardening, and worth **starting now, in parallel with Stages 2–11**, since
it's on the critical path to inviting anyone at all (unlike Stage 14, which
your answer explicitly said can wait):

- **Technology: AMD SEV-SNP**, decided over both Intel TDX and AWS Nitro
  Enclaves. `docs/DESIGN.md` §6.1 already rules out SGX ("secret-scalar EC
  multiplication — exactly what scanning does — is precisely what SGX's
  published side-channel attacks target"); between the two real remaining
  options, SEV-SNP and TDX are both, as of 2026, sold as confidential VMs by
  AWS, Azure, and GCP — so unlike this doc's earlier framing, the choice no
  longer decides which cloud we're locked into (that was true of AWS Nitro
  Enclaves specifically, which is why it's no longer in the running: it's
  AWS-proprietary infrastructure, not a CPU feature, and would have locked
  Stage 1's hosting to one vendor for no compensating benefit). SEV-SNP wins
  on a concrete architectural fit: it has **VMPL** (VM Privilege Levels),
  letting a guest partition itself into hardware-enforced privilege tiers —
  which maps directly onto this plan's split-process design below, isolating
  the key-custody component even from a compromise of the guest's *own*
  kernel, not just from the hypervisor outside it. TDX has an analogous
  "TD partitioning" concept but it's newer and less available/battle-tested
  across providers. Worth being honest that this isn't a slam-dunk: SEV-SNP
  had a real, serious isolation-breaking vulnerability in 2025 (StackWarp,
  CVE-2025-29943 — patched by AMD in July 2025), and both SEV-SNP and TDX
  were broken alike by "TEE.fail," a ~$1,000 DDR5 memory-bus physical
  interposer attack — but that's a physical-access attack against the
  actual hardware, a much higher bar than `docs/DESIGN.md` §6.1's actual
  threat model (rogue admin, compromised hypervisor, remote exploit), so it
  doesn't change the recommendation. Whichever cloud is chosen, confirm its
  SEV-SNP instances have AMD's July 2025 microcode patch before treating
  this as production-ready.
- **Architecture: a confidential VM plus a process-level split, not a full
  enclave rewrite.** This is the other correction from this doc's earlier
  framing, which had assumed Nitro Enclaves' split-VM model (a separate
  enclave image, vsock IPC, no direct network/disk from inside it) was the
  only shape this could take. SEV-SNP protects a *whole guest VM's* memory
  from the host/hypervisor — so the existing `moneropay-core` binary can run
  largely as-is inside that VM, already covering "a compromised host can't
  read tenant keys." What a confidential VM alone does *not* cover is a
  remote exploit of the same big, internet-facing process (the HTTP layer,
  JSON parsing, webhook delivery, every dependency) reading its own memory —
  so key custody still needs to live in a separate, minimal process inside
  that VM: a small `key-custody-service` owning the actual view keys and
  exposing only the `KeyCustody` trait's operations (`register_wallet`,
  `seal`/`unseal_and_register`, `derive_subaddress`, `scan_tx_outputs`) over
  a local Unix socket, with the main engine process as a client rather than
  a keyholder. Ordinary OS process isolation (separate user, seccomp, no
  shared memory) is the floor this needs regardless; SEV-SNP's VMPL is an
  optional hardware-enforced upgrade to that same boundary, worth using if
  the implementation effort is reasonable, not a blocking requirement.
- **The shape of the work**: a new `KeyCustody` implementation (the
  `key-custody-service`, talked to over a socket instead of in-process) is
  real new code, but meaningfully less than an enclave-split model would
  have needed — no vsock plumbing, no attested-image build pipeline, just a
  second small binary and a socket protocol. `seal` needs to produce bytes
  only that service can `unseal` (§6.2 point 2 already requires this —
  `PlainKeyCustody` deliberately doesn't seal today, so this part is
  genuinely new regardless of the hardware). `PlainKeyCustody`'s existing
  test suite (`src/key_custody/plain.rs`, per §6.3) is the right behavioral
  contract to run against the new implementation — same trait, same
  expected outputs — so this isn't "invent a new way to test key custody,"
  it's "prove a second implementation of the trait we already have a spec
  for."
- Still genuinely more engineering than anything else in this plan short of
  the plugin itself — worth sizing it honestly as its own effort, but
  smaller than this doc previously estimated now that it doesn't require
  Nitro's split-VM model.

### Stage 13 — Hosted-specific hardening (gate before public launch, not before private beta)

Everything here *other* than key custody, which now has its own gate above:

- Backups of the engine's SQLite file (it holds every tenant's sealed key
  material and full order history) with a tested restore procedure, not
  just a cron job nobody's verified.
- Basic incident runbook: what happens, and what we tell merchants, if the
  box is compromised — losing a view key is a privacy incident (who paid
  whom, how much, when), never a funds-loss one, per the engine's core
  design guarantee, and that distinction should be in the runbook explicitly
  since it changes the severity and disclosure conversation. With Stage 12
  done, this incident class should already be much harder to trigger, but
  the runbook is still worth having.

### Stage 14 — Legal/compliance (parallel track, waits until the end)

Running a hosted service that touches merchants' payment flows, even
non-custodially (no spend key ever exists, per the engine's core design
guarantee), is worth a real look at money-transmission/licensing exposure in
whatever jurisdiction(s) this launches from, before public marketing rather
than after. Per your answer: since launch is to a small private group first,
this explicitly does **not** need to be finished before that — it's a
blocker on going public/wider, not on inviting the initial beta group, so it
can genuinely wait until the end rather than running in parallel with
everything else the way it's often treated. Worth someone owning it as a
background task regardless, so it isn't a surprise scramble once public
launch is actually on the table. One added wrinkle worth a note here: if the
business is structured as a not-for-profit (per your answer on
monetization), that structure itself may need to exist before the §6
anonymous-access fee can be collected at all — worth folding into whatever
this workstream produces, not treating as a separate step.

### Stage 15 — Shopify readiness check (not building yet)

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

## 6. Parked for later: an anonymous "pay by Monero" account at the control
plane layer

Not being built as part of this MVP — recorded here so the idea isn't lost,
and so a future pass doesn't have to rediscover why it's shaped this way.

The problem it would solve: §4's Option A gives the business a clean
accounts story, but it does mean *every* hosted tenant needs an email and a
password, even for someone who'd rather not have an account at all. The
fix isn't to reopen the engine's admin API (that's exactly what Option A
closed, for good reason) — it's to let the *control plane* mint a
lightweight, anonymous identity of its own: something like a bearer API key
handed out with no email required, scoped to one (or a few) tenants,
functionally "an account with no login and no recovery story." The control
plane still creates the underlying engine tenant exactly the way Stage 3
does today (so Option A's "every tenant is control-plane-linked" guarantee
never breaks) — the only thing that changes is what counts as "an account"
one level up, at the control plane's own `users` table (or a sibling table
for anonymous identities, so real accounts and anonymous ones aren't
conflated).

One question this already has an answer to, per your latest note: the
throttle on abuse is a **fee**, not a rate limit or a PoW challenge — this
tier is the one place in an otherwise not-for-profit, free service where
money changes hands, specifically because charging something is a more
reliable deterrent against unlimited free tenant creation than any
technical throttle this doc considered under Options B/C. Worth noting the
nice thematic fit for whenever this is built: the fee for anonymous access
to a Monero payment gateway is presumably itself charged in Monero, which
keeps the anonymous path anonymous end-to-end rather than quietly
requiring a credit card (and the identity that comes with one) to reach it.

Still open for whenever this gets picked up: whether an anonymous identity
can later be upgraded to a real email-linked account without migrating its
tenants; what the fee amount/structure actually is (one-time per identity?
per tenant? recurring?); and whether it's exposed as a first-class signup
option or stays a deliberately-unadvertised "advanced users" path.

## 7. Open questions for you

Axum, beta scope, high-level monetization, and the TEE technology/
architecture choice (SEV-SNP, confidential VM + process split) are all
settled as of your last two answers — thank you. What's left:

1. **How deep should Stage 12 go in this document?** I've sketched it at
   the same level as everything else here — what it is, why, what it
   depends on — but a real confidential-VM deployment plus a new
   `key-custody-service` (attestation, the socket protocol, key sealing) is
   a meaningfully bigger and more specialized effort than the WooCommerce-
   side stages. Worth a dedicated design document of its own now that the
   technology is settled, or is the current level of detail enough to start
   from?

## 8. What I'd build first if you say go

Two independent tracks, both startable immediately:

- **Track A (WooCommerce protocol)**: Stage 2 (workspace restructuring —
  small, mechanical, unblocks everything else) followed by Stage 3 and the
  first half of Stage 6 (a bare `POST /connect/woocommerce/start` +
  `/finish` pair against a stubbed wallet form, no real dashboard yet) —
  proves the connect mechanism end to end before any WooCommerce-specific
  code, mock or real, exists.
- **Track B (go-live gate)**: Stage 12's `key-custody-service` on SEV-SNP —
  it has no dependency on Track A and, per your answer, is the thing most
  likely to actually gate when a private beta can start, so it shouldn't be
  sequenced after the WooCommerce work by default.
