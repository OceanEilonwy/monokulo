# FX / Checkout Refactor — Work Breakdown Structure

Moves fiat exchange-rate handling and the embeddable checkout/payment UI
out of the engine (`moneropay-core`) entirely, into the control-plane. The
engine becomes strictly Monero-watching: given a wallet to watch and a
piconero amount to expect, it tells you when it's paid — no FX, no
merchant-facing HTML. Decided in-session (see `work_notes.md`): a
self-hoster who runs the engine *without* the control-plane is accepted as
a power user who writes their own fiat/checkout integration — this is a
deliberate scope narrowing, not an oversight.

This file exists because the migration is large and touches almost every
layer of both crates — read this before starting any leaf below, and keep
it in sync with `work_notes.md` (that file is the running "what's actually
done" record; this file is the plan and doesn't change as work completes,
same convention `docs/WOOCOMMERCE_WBS.md` already established).

## Why this shape, not a bigger-bang rewrite

Each phase below is picked to be independently shippable and independently
testable — never "half migrate the engine's order API and hope the rest
follows same-day." The one hard cutover is Phase 3 (the engine's public
order-creation API stops accepting fiat) — everything before it can ship,
run, and be verified against the *current*, unmodified engine; everything
after it depends on that cutover having landed. Do not skip ahead to
Phase 3 to "save a step" — the whole point of Phases 1–2 is proving the
control-plane side works before anything on the engine side becomes
irreversible for real deployments.

## Open decisions — resolve these before or during Phase 1, not silently

These are real product calls this document does not make on its own
behalf. Each names a recommendation, but the recommendation is not a
decision.

1. **Per-tenant checkout template customization** (`tenants.template_dir`,
   `TemplateEngine::new(custom_dir)` reading a merchant-supplied
   `checkout.html.hbs` off disk — `src/templates.rs`/`src/store.rs` at the
   repo root) has no equivalent in a hosted, database-backed control-plane
   (there is no per-tenant filesystem to read from). Options: (a) drop it,
   every control-plane checkout page looks the same, on-brand with the
   rest of the site (matches the direction the whole control-plane visual
   identity has taken this session); (b) replace it with a much narrower,
   DB-backed customization (a logo URL, an accent color) rather than
   arbitrary HTML. **Recommendation: (a) now, revisit (b) only if a real
   merchant asks.**
2. **Does the engine keep `fiat_amount`/`fiat_currency`/`exchange_rate` as
   opaque, unvalidated passthrough fields on the order row** (still
   accepted and echoed back, just never computed/interpreted by the
   engine), **or are they removed from the engine's schema and API
   entirely**, with control-plane keeping its own fiat record locally,
   joined by `payment_id`? A passthrough field is less work now but
   contradicts "engine has no concept of FX" — a string the engine stores
   but never validates is still a small FX-shaped concept living there.
   **Recommendation: remove them from the engine entirely (real schema
   migration, Phase 3) — control-plane owns this data fully, including
   the durability trade-off that implies (see decision 4).**
3. **`static/moneropay-client.js`** (the merchant-embeddable JS library,
   `src/http/public.rs::client_library`) has a fiat-aware `createOrder()`
   today. Does this move to being served *from* control-plane (its
   `createOrder` calls control-plane's new order-creation endpoint), or
   does the engine keep serving a much thinner, XMR-only version of it for
   the power-user self-host case? **Recommendation: move it to
   control-plane entirely, alongside the checkout page it exists to embed
   — a self-hoster without control-plane is already expected to write a
   custom integration per this refactor's own stated scope, so a thinner
   engine-side JS library would serve nobody the WBS actually targets.**
4. **Durability of control-plane's own fiat records.** Once fiat data
   lives only in control-plane's database (decision 2), it's no longer
   recoverable from the engine if control-plane's own DB is ever lost —
   the engine would still truthfully report XMR amounts and payment
   status, but the fiat price/currency a customer was originally quoted
   would be gone. Confirm this is an acceptable trade for how
   control-plane's database is actually operated (backups, etc.) before
   Phase 3 removes the engine-side copy.
5. **API versioning for the breaking change.** This project has no
   production traffic on the current fiat-aware `POST /api/v1/t/{pk}/orders`
   contract yet (`docs/WOOCOMMERCE_WBS.md`'s own framing: pre-private-beta).
   **Recommendation: break the contract in place, no `/api/v2/` prefix —
   document it loudly (changelog, this file, `docs/DESIGN.md`) instead of
   carrying version-negotiation complexity for zero real consumers.**
   Revisit if that assumption turns out to be wrong.

---

## 0. Foundations

- 0.1 Extract the engine's rate limiter to `shared`
  - outcome: `shared::rate_limit::RateLimiter<K>` (generic bucket key,
    exactly the shape `src/http/rate_limit.rs` at the repo root already
    grew into this session for the public/admin split); both the engine
    and control-plane depend on it
  - what: move `RateLimiter`/`LimiterState`/the pruning logic and its 9
    existing unit tests into `shared`; the engine's own
    `rate_limit_middleware`/`admin_rate_limit_middleware` become thin
    wrappers calling into it, same "cut and re-import" pattern
    `docs/WOOCOMMERCE_WBS.md` 0.2/0.3 already used for `shared::auth`/
    `shared::webhook_sign`
  - why now, not in Phase 1: Phase 1 needs this for control-plane's new
    public order-creation endpoint (see 1.4) — building it twice (once
    ad hoc in control-plane, once properly later) is waste
  - test: existing `RateLimiter` unit tests move and pass unmodified from
    `shared`; engine's own `cargo test --workspace` unaffected in
    count/behavior

## 1. Control-plane gains FX (engine untouched, ships independently)

- 1.1 Add an exchange-rate provider to control-plane
  - outcome: `control_plane::exchange_rate` with the same
    `ExchangeRateProvider` trait shape as the engine's own
    (`piconero_per_unit(&self, currency) -> Option<u64>`), `FixedRateProvider`
    and `CoingeckoRateProvider` implementations
  - what: this is real, working logic already proven in the engine
    (`src/exchange_rate.rs` at the repo root, 647 lines, live-verified
    against the real Coingecko API this session) — move it to `shared`
    (not control-plane directly) so both crates *could* still use it
    during the transitional window, then have the engine stop depending
    on it once Phase 3 lands. `compute_xmr_amount`/`format_piconero_as_xmr`
    (exact-integer fiat-decimal-string → piconero conversion, §8.1's
    "money is never a float" rule) move the same way.
  - what (config): control-plane needs its own `[exchange_rate]`
    configuration surface (provider choice, currencies, cache seconds,
    fixed rates) — likely per-instance for now (decision-4-adjacent: is
    this ever per-tenant? Out of scope here; today's ask was "control-plane
    owns FX lookups," not "each tenant picks a different rate source" —
    confirm this reading before building anything fancier)
  - test: move the engine's existing `exchange_rate.rs` tests verbatim
    (including the live-Coingecko-API-shape test and the float-boundary
    tests) — they test the logic, not which crate it lives in
- 1.2 Control-plane's own local fiat-order metadata store
  - outcome: a new table (e.g. `order_fiat_metadata`, keyed by
    `(connection_id, payment_id)`) recording `fiat_amount`/`fiat_currency`/
    the `piconero_per_unit` rate used at creation time — the record the
    engine's own `orders` table currently holds, now owned here instead
  - what: `control_plane::db` gains the table + migration + insert/lookup
    methods; this is genuinely new state control-plane didn't need before
    (everything today is a live proxy over the engine's own stored data —
    see `control-plane/src/engine_client.rs`'s own module doc comment on
    that being the deliberate design up to now)
  - test: real sqlite round-trip tests, same style `control-plane/src/db.rs`
    already uses elsewhere in this crate
- 1.3 Control-plane's own public rate-limiter, wired to the new endpoint
  below
  - outcome: `control-plane`'s `AppState` gains a `rate_limiter` field
    (there is none today — control-plane has never needed one, since it's
    never served an unauthenticated, state-changing public endpoint before
    now)
  - what: reuse `shared::rate_limit` from 0.1; apply it as middleware only
    to the new public order-creation route (1.4) — control-plane's
    existing authenticated `/dashboard/*` and admin-proxy routes don't need
    it, same reasoning the engine's own public/admin split already
    established this session
  - test: same shape as the engine's own
    `rate_limit_middleware_rejects_after_the_limit_with_a_real_connect_info`
- 1.4 New public, unauthenticated order-creation endpoint on control-plane
  - outcome: `POST /pay/{connection_id}/orders` (or similar — name it to
    not collide with `/dashboard/*`) accepts `fiat_amount`/`fiat_currency`,
    computes the XMR amount via 1.1, calls the engine's *existing*
    fiat-aware `create_order` (unchanged at this point — Phase 3 hasn't
    landed), records the fiat metadata via 1.2, returns a control-plane-
    owned response
  - what: this deliberately reuses and generalizes the "create a test
    order" feature already shipped this session
    (`control-plane/src/http/orders.rs::create_order`,
    `EngineClient::create_order`) — that button becomes the real production
    path for every fiat-priced order once this is done, not a separate
    thing
  - why this order (engine still fiat-aware here): proves control-plane's
    own FX computation, storage, and rate-limiting all work for real,
    before anything about the engine's own contract has to change
  - test: real end-to-end test against a real spawned engine (same
    `engine_test_support` pattern every other control-plane test already
    uses) — a real order created through *this* new endpoint, with the
    right XMR amount and the right fiat metadata recorded locally

## 2. Control-plane's own checkout/payment page (iframe-able)

- 2.1 Move QR code rendering to control-plane
  - outcome: `qrcode` crate as a control-plane dependency; the same
    `qr_svg_for_html` trimming/accessibility logic
    (`src/http/public.rs::qr_svg_for_html` at the repo root — strips the
    non-HTML-valid `<?xml?>` prolog, marks the SVG `aria-hidden`/
    `role="presentation"`/`focusable="false"`) moved over verbatim
  - test: the existing rendering/accessibility assertions move with it
- 2.2 Build the control-plane checkout page
  - outcome: `GET /pay/{connection_id}/orders/{payment_id}` (public,
    unauthenticated, iframe-able — no `X-Frame-Options`/`frame-ancestors`
    restriction, unlike every other control-plane page) renders address,
    QR code, XMR amount, the fiat amount/currency from 1.2's local record,
    payment/confirmation status, live-polls for updates
  - what: `EngineClient::get_order_status`-equivalent call for live
    status (the engine's *existing* `GET /api/v1/t/{pk}/orders/{payment_id}`
    already returns no fiat fields at all — confirmed by reading
    `OrderStatusResponse`, `src/http/public.rs` at the repo root — so this
    endpoint needs zero engine-side change to support the new checkout
    page); styled with the control-plane's own `_styles.html.hbs` design
    system, not a byte-for-byte copy of the engine's old
    `templates/default/checkout.html.hbs`
  - what (client-side polling): port the existing polling JS from the
    engine's checkout template rather than re-inventing it — it's already
    correct, tested behavior (`templates::tests::the_poll_loop_checks_response_ok_before_reading_a_status`
    and friends, `control-plane/src/templates.rs`, already has an
    equivalent pattern from the status page's own polling this session)
  - test: real HTTP-level test rendering the page against a real order on
    a real spawned engine, asserting address/QR/amounts/status all present
    and correct; a second test for the live-status-changes-on-poll path

## 3. The breaking change: engine order-creation API becomes XMR-only

**Do not start this phase until Phases 1–2 are shipped and verified live.**
Everything here is the point of no return for any existing integration
against the engine's current public API.

- 3.1 Engine: `POST /api/v1/t/{pk}/orders` drops `fiat_amount`/
  `fiat_currency`, requires `xmr_amount_piconero` directly
  - outcome: `CreateOrderRequest`/`CreateOrderResponse`
    (`src/http/public.rs` at the repo root) carry no fiat fields at all;
    `create_order` no longer touches `state.exchange_rate` or
    `compute_xmr_amount`
  - what: `AppState.exchange_rate: Arc<dyn ExchangeRateProvider>` is
    removed entirely — every construction site (`main.rs`,
    `engine-test-support`, `src/http/tests.rs`, both e2e tests) loses this
    field, same mechanical-but-wide blast radius the rate-limiter field
    addition had this session, just in reverse
  - test: existing `create_order`-family tests in `src/http/tests.rs`
    rewritten to pass `xmr_amount_piconero` directly; delete tests that
    were purely about fiat-to-XMR conversion correctness (that logic now
    lives, and is tested, in `shared`/control-plane per Phase 1)
- 3.2 Engine: drop `fiat_currency`/`fiat_amount`/`exchange_rate` from the
  `orders` table
  - outcome: a real schema migration (next number after whatever
    `migrations/000N_*.sql` currently ends at) removing the three columns;
    `NewOrder`/`OrderRow` (`src/store.rs` at the repo root) lose the fields
  - what: this is the single largest mechanical-effort item in the whole
    migration — confirmed via `grep -c "fiat_currency:" src/store.rs
    src/scanner.rs`: 8 and 6 respectively, 14 separate
    `NewOrder { fiat_currency: ..., fiat_amount: ..., exchange_rate: ... }`
    literals across those two files' own test fixtures alone (not
    counting `src/http/public.rs`/`admin.rs`'s real, non-test call sites) —
    budget real time for this, it's not a one-line diff multiplied by 14,
    each site needs its own literal edited
  - what (admin API): `OrderView` (`src/http/admin.rs` at the repo root,
    what control-plane's own `EngineClient::OrderView` DTO mirrors) drops
    the same three fields — `control-plane`'s orders-list/order-detail
    pages, which currently read `fiat_amount`/`fiat_currency` straight off
    this DTO, switch to reading from control-plane's own local table
    (1.2) instead, joined by `payment_id`
  - test: every existing store/scanner test touching `NewOrder` updated;
    a fresh migration test (apply-then-verify-column-gone, matching this
    repo's own existing migration-test conventions in `src/store.rs`)
- 3.3 Control-plane: point 1.4's endpoint at the now-XMR-only engine API
  - outcome: `EngineClient::create_order` sends `xmr_amount_piconero`
    (computed by control-plane itself, per 1.1) instead of
    `fiat_amount`/`fiat_currency`
  - test: re-run 1.4's own end-to-end test against the now-updated engine
    — same assertions, now exercising the real, final call shape

## 4. Remove the engine's checkout UI and exchange_rate module entirely

- 4.1 Delete `src/http/public.rs::payment_page`, `qr_svg_for_html`,
  `templates/default/checkout.html.hbs`, `CheckoutViewModel`/
  `PaymentViewModel` (`src/templates.rs` at the repo root), the
  per-tenant `template_dir` column/config (decision 1)
  - outcome: `GET /pay/v1/{pk}/{payment_id}` no longer exists on the
    engine at all — it's `GET /pay/{connection_id}/orders/{payment_id}` on
    control-plane now (2.2)
  - what: a real schema migration removing `tenants.template_dir`;
    `TenantConfigPatch`'s `template_dir_set`/`template_dir` fields go with
    it; every one of `templates.rs`'s ~15 `render_checkout`-based tests
    (custom-template-override, double-spend-banner, confirmation-progress-
    bar, payments-table-caption, accessibility tests — real, substantial
    test coverage) is deleted, not just the production code — confirm
    nothing in that list was quietly testing chain-scanning/store logic
    disguised as a template test before deleting wholesale
  - test: `cargo test --workspace` — the deletion itself is verified by
    the absence of compile errors and a clean, smaller test count, not a
    new test
- 4.2 Remove `src/exchange_rate.rs`, `AppState.exchange_rate`,
  `[exchange_rate]` config section, the Coingecko-refresh background loop
  (`main.rs::run_coingecko_refresh_loop`/its `supervise` call)
  - outcome: the engine has no FX code, no FX config, no FX network calls,
    anywhere
  - what: `Config::validate`'s `exchange_rate.provider`/`.currencies`/
    `.cache_seconds`/`.rates` checks (`src/config.rs` at the repo root) go
    with it; `docs/DESIGN.md` §13's `[exchange_rate]` config-surface
    sketch needs updating in the same change (see Phase 6)
  - test: `cargo test --workspace` clean; confirm no `exchange_rate`
    string survives outside this file's own history/changelog mentions
- 4.3 Remove `src/http/public.rs::client_library`/
  `static/moneropay-client.js` from the engine (decision 3)
  - outcome: `GET /static/moneropay-client.js` no longer exists on the
    engine
  - what: the file (and a rewritten, XMR-only-or-nonexistent successor,
    per decision 3) moves to control-plane's own static assets, serving
    its `createOrder()` against control-plane's new 1.4 endpoint instead
    of the engine directly
  - test: whatever control-plane's own equivalent route test looks like
    (real HTTP GET, correct `Content-Type`, script actually calls the
    right endpoint - a real integration test if control-plane's own
    dashboard or a mock storefront actually exercises it, not just "the
    bytes are served")

## 5. e2e test rework

- 5.1 `tests/e2e_stagenet.rs` (engine-only, real-money-costing,
  `#[ignore]`d) — creates orders directly against the engine's public API
  today; switch to `xmr_amount_piconero` directly, dropping the fiat
  fields it currently sends
- 5.2 `tests/e2e_dashboard_stagenet.rs` and
  `mock-woocommerce/tests/e2e_stagenet_connect_flow.rs` — both currently
  create fiat-denominated orders; rework to go through control-plane's
  new 1.4 endpoint instead of the engine directly, since that's now the
  *real* path a production merchant integration takes — more faithful
  coverage than hitting the engine's own (now XMR-only, developer-facing)
  API, not just a mechanical fiat→XMR find-and-replace
  - why this matters more than it looks: this is the first time either of
    these e2e tests would exercise control-plane's *own* order-creation
    surface rather than only the engine's — a real, new gap closed, not
    busywork

## 6. Documentation

- 6.1 `docs/DESIGN.md`: §5 (High-Level Architecture — checkout page moves
  off the engine's own diagram), §8 (Data Model — `orders` table schema
  change), §10 (HTTP API Surface — `/pay/v1/...` removed, `create_order`
  contract changed), §13 (Configuration Surface — `[exchange_rate]`
  section removed), §14 (Client Library — moved to control-plane or
  rewritten per decision 3)
- 6.2 `control-plane/templates/landing.html.hbs`'s own marketing copy
  ("MoneroPay Cloud runs the same open, self-hostable engine either way")
  becomes less accurate once this ships — reword to something like "the
  same open-source, self-hostable payment-watching engine" rather than
  implying full feature parity between self-hosted-alone and hosted
- 6.3 `work_notes.md`: a real entry once each phase lands (same practice
  every other multi-session piece of work in this repo already gets), not
  just a note when the whole thing is done
