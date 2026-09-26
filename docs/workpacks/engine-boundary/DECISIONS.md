# Decisions: Monokulo–engine boundary work pack

Every place the plan in `README.md` left a detail open, or where the work had to deviate. Numbered; newest last.

Format for each entry:
- **Step:**
- **Decision:**
- **Alternatives considered:**
- **Why:**

### 1. Public bind is warned about, not refused
- **Step:** 1
- **Decision:** A non-private `server.bind` prints a `WARNING` to stderr at boot (after binding, using the listener's real local address) but still starts. Private means loopback, RFC 1918, IPv6 ULA `fc00::/7`, or link-local (`169.254/16`, `fe80::/10`); IPv4-mapped IPv6 is judged by its IPv4 part. `0.0.0.0`/`::` and CGNAT `100.64/10` count as public.
- **Alternatives considered:** Refuse to start on a public bind unless an override setting is set; also classify CGNAT as private.
- **Why:** The plan asks for a loud warning, not a refusal, and some operators run the engine on a separate host in a private network reachable only by monokulo. Classifying from the listener's local address (not the setting string) also handles hostnames like `localhost:8443`. CGNAT space is shared with other ISP customers, so it is not private to the operator.

### 2. `allowed_origins` on `POST /connections` is accepted as an alias for `domains`
- **Step:** 3
- **Decision:** The JSON field is now `domains` (optional, default empty), with `#[serde(alias = "allowed_origins")]`. An old caller's `allowed_origins` values are treated exactly like `domains`: each joins the store's domains waiting for DNS. Sending both names is a `4xx` (serde duplicate-field error). Each entry may be a bare domain (`shop.example`) or an origin/URL (`https://shop.example`); onion addresses and IPs are skipped quietly, as before (new `embed_domains::suggest_domain`).
- **Alternatives considered:** Ignore `allowed_origins` silently; reject it with a `400`.
- **Why:** The values an old caller sent were always meant as "sites my checkout runs on", which is exactly what `domains` means now, so honouring them loses nothing and breaks nobody (the stagenet test and any external script still send `allowed_origins`). Ignoring it would silently drop the caller's intent. Tested by `the_old_allowed_origins_field_is_accepted_as_an_alias_for_domains`.

### 3. The one-time domain import runs synchronously at startup
- **Step:** 3
- **Decision:** `embed_domains::import_existing_domains(db)` is now a plain function called directly in `main.rs` before the server starts, instead of a spawned async task.
- **Alternatives considered:** Keep it spawned.
- **Why:** It no longer makes network calls (nothing is read from the engine), only a few local SQLite writes, once per store ever. Running it before serving means a store's site domain is present from the first request.

### 4. `EngineClient`'s `CreateTenantRequest` keeps an always-empty `allowed_origins` until step 8
- **Step:** 3
- **Decision:** Monokulo still serializes `allowed_origins: []` when creating a tenant, because the engine's create request requires the field until step 8 removes it. `TenantView` and `PatchTenantRequest` lost the field now.
- **Alternatives considered:** Make the engine field optional in step 3.
- **Why:** Keeps engine changes in step 8 where the plan puts them; sending an empty list is "creating with an empty `allowed_origins`", which the plan asks for.

### 5. The secret key is recognised on every `/pay/{pk}/...` route, checked in its own middleware
- **Step:** 4
- **Decision:** `http::store_key::store_key_middleware` runs first on the `/pay/...` sub-router (after CORS). Any request with an `Authorization` header is checked against the store in the path: the right key marks the request (`StoreKeyAuthenticated` extension) and spends a per-store budget instead of the per-IP one; anything else (wrong key, another store's key, a non-`Bearer` value, an unknown store) gets `401` with a JSON error, and the failed attempt spends the caller's per-IP budget. The comparison uses `subtle::ConstantTimeEq` on the decrypted stored key (new direct dependency `subtle = "2.6.1"`, already in `Cargo.lock` transitively).
- **Alternatives considered:** Check the key only inside `pay::create_order`; only on `POST /pay/{pk}/orders`.
- **Why:** The rate limiter and the embed-policy middleware both need to know about the key before the handler runs, so one middleware decides once. Recognising it on the status route too lets a shop's server poll status under its own budget (the plan's matrix: "keyed by store (key)" for the JSON API). Browsers can't send `Authorization` cross-origin (CORS doesn't allow it), so embedding pages are unaffected. Charging the per-IP budget for failures bounds key guessing.

### 6. Per-store key limit: 600 a minute, a boot-time setting
- **Step:** 4
- **Decision:** New setting `rate_limit.per_store_key_per_min` (`MONOKULO_RATE_LIMIT_PER_STORE_KEY_PER_MIN`), default `600`, at least 1, read at boot like the per-IP limit, editable on the admin settings page.
- **Alternatives considered:** 120/min (the engine's old per-token default); unlimited.
- **Why:** A shop's server sends every customer's order-creation (and maybe status polling) from one address; 600/min (10/s) is far above a normal shop's checkout rate while still capping a leaked or runaway key. Step 9 folds this into the tiered design.

### 7. Orders recorded before migration 0021 count as created with the key
- **Step:** 4
- **Decision:** `order_currency_metadata.created_with_key INTEGER NOT NULL DEFAULT 1`; every new insert sets it explicitly (`pay::create_order`: whether the key was presented; dashboard and POS: `true`).
- **Alternatives considered:** Default `0` for existing rows.
- **Why:** Which path created an old order isn't recorded. Step 7 hides browser-created orders of restricted stores outside a frame; defaulting old rows to "not keyed" could break checkout links merchants already sent out. Old browser-created orders on restricted stores (phase 2 shipped days earlier) are a small, shrinking set.

### 8. The missing-`public_url` check happens on the confirm screen, on its submission and in `/finish`
- **Step:** 5
- **Decision:** While `public_url` is unset, `GET /connect/{platform}` shows the confirm page with the reason and no form (so the merchant learns early), `POST /connect/{platform}` re-renders that page without creating anything, and `POST /connect/{platform}/finish` answers `503` with `{"error": "..."}` *before* redeeming the token (so the same token still works once the operator sets the address).
- **Alternatives considered:** Only `/finish` (the merchant would only find out after entering keys); only the confirm screen (a token minted just before the setting was cleared would hand out a wrong address).
- **Why:** The plan asks for the merchant to see it early and for plugins never to get a wrong address; checking in all three places does both. `503` fits "this instance isn't ready", and a JSON body lets the plugin show the message.

### 9. `public_url` validation and normalisation
- **Step:** 5
- **Decision:** New monokulo setting `public_url` (`MONOKULO_PUBLIC_URL`, default empty = unset). Valid: an absolute `http`/`https` URL with a host and nothing after it but an optional `/` (no path, query, fragment or login). Stored as typed; read through `settings::public_url()`, which trims the trailing `/` and treats an invalid value (possible only via the environment variable) as unset with a log line. The admin page refuses invalid values and shows help text (new `help` field on `AdminScalarFieldView`, from `settings::help`), which step 9e reuses.
- **Alternatives considered:** Allow a path prefix (monokulo behind a sub-path).
- **Why:** The plan says no path beyond `/`; monokulo's routes and `monokulo-client.js` assume they sit at the root.

### 10. `integration_help` shows the public address; `store_connections.moneropay_endpoint` is left in the database
- **Step:** 5
- **Decision:** `integration_help::fragment` now takes `public_url: Option<&str>` instead of the unrendered engine endpoint, and uses it to make the widget and API snippets absolute when set (relative, as before, when not). The views' `endpoint` fields are gone. The `store_connections.moneropay_endpoint` column is still written (the engine URL) and read into `StoreConnectionRow`, but nothing renders or returns it.
- **Alternatives considered:** Drop the column with a migration; keep the relative snippets only.
- **Why:** Absolute URLs are what a merchant pasting into another site needs. The column is internal (never sent to a merchant or plugin) and dropping it is unrelated schema churn; its existence doesn't break "nothing a merchant or plugin receives contains the engine's address".

### 11. Old WooCommerce installs are detected by a missing `connection_version` setting
- **Step:** 6
- **Decision:** `WC_Gateway_Monokulo::CONNECTION_VERSION = '2'`. `process_connect_return()` writes `connection_version = '2'` after a successful connect. An install with any credential (`endpoint`, `public_key` or `secret_token`) but a different or missing `connection_version` "needs reconnect": `is_available()` is false, `create_monokulo_order()` refuses without any HTTP call, the gateway's settings field says so, and a site-wide `admin_notices` warning (hooked in `monokulo.php`, shown to `manage_woocommerce` users) links to the settings page. A store that never connected gets no reconnect notice. Only the connect flow writes the marker; hand-editing the settings form doesn't.
- **Alternatives considered:** Compare the stored endpoint with something (rejected by the plan: no URL guessing); set the marker whenever the settings form is saved (a merchant pressing Save on an old install would silently re-mark the engine address as current).
- **Why:** The marker states a fact the plugin knows for certain (this connection came from the new `/finish`), and the check is a pure function of the stored settings, easy to test. Self-hosters can still use the connect flow via the existing `monokulo_control_plane_base_url` filter.

### 12. Plugin amount formatting and error handling
- **Step:** 6
- **Decision:** The plugin sends `amount` with 2 decimals for fiat (Monokulo refuses more) and, for an `XMR`-priced store, the store's price decimals clamped to 2..12. `description` is no longer sent (Monokulo's request has no such field). Monokulo errors are logged with status, Monokulo's `error` text and a hint (401: reconnect, 403: verified-domain settings, 404: unknown store, 429: rate limited); the customer sees one of three generic messages (busy for 429, "not available for this store" for 401/403, "could not start this payment" otherwise).
- **Alternatives considered:** Always 2 decimals (loses XMR precision); show Monokulo's error to customers (leaks store configuration).
- **Why:** Matches Monokulo's `compute_order_amount` rules and keeps customer-facing text safe.

### 13. How the PHP suite was run
- **Step:** 6
- **Decision:** `npx @wordpress/env start` fails here: the plugin's directory is named `woocommerce` (since commit `a5491cf`), so wp-env mounts it over WooCommerce's own `wp-content/plugins/woocommerce` and WooCommerce disappears ("requires 1 plugin"); Node's 250 ms happy-eyeballs timeout also broke its downloads until `NODE_OPTIONS=--dns-result-order=ipv4first --network-family-autoselection-attempt-timeout=5000`. I ran the suite with wp-env's generated `docker-compose.yml`, copied to the session scratchpad with this plugin mounted at `wp-content/plugins/monokulo` instead, plus a `wp-tests-config.php` copied from another wp-env instance: `docker compose -f <copy> -p monokulo-phpunit run --rm -w /var/www/html/wp-content/plugins/monokulo tests-cli vendor/bin/phpunit`.
- **Alternatives considered:** Rename the plugin directory in the repo (out of scope, affects packaging); skip the PHP suite (the plan prefers running it).
- **Why:** Runs the real suite against real WordPress/WooCommerce without changing the repository layout. The collision itself is a pre-existing problem worth fixing separately (e.g. a `.wp-env.json` `mappings` entry).

### 14. Frame-only rule: 403 page, share page included, `/status` and `/events` exempt
- **Step:** 7
- **Decision:** For a restricted store's browser-created order (`created_with_key = 0`) requested with a `Sec-Fetch-Dest` other than `iframe`/`frame`, `GET /pay/{pk}/orders/{id}` answers `403` with a plain server-rendered "Open this payment from the shop's website" page (`views::checkout::open_from_shop_page`, `layout_bare` like the not-found page, no script, no payment details). The same rule applies to `GET /pay/{pk}/orders/{id}/share`. `/status`, `/events` and the refund-address POST are not restricted. Responses of both pages carry `Vary: Sec-Fetch-Dest`. Orders monokulo has no metadata for are let through.
- **Alternatives considered:** `200` for the refusal page; leaving `/share` alone; restricting `/status` and `/events` too.
- **Why:** `403` says the content exists but isn't served here. `/share` frames the checkout from monokulo itself (allowed by `frame-ancestors 'self'`), so leaving it open would give every browser-created order a full-page URL and defeat the rule; key-created orders (what the dashboard shares) still pass. `/status` is fetched cross-origin by `monokulo-client.js` on the merchant's page and `/events` by the framed page itself (`Sec-Fetch-Dest: empty` for both), and they carry no page a customer could be sent to, so restricting them would break the embed without closing anything. The refund POST comes from inside the frame and redirects back to the checkout page, which is checked.

### 15. Playwright coverage for step 7 checks the browser premise, not monokulo
- **Step:** 7
- **Decision:** The new surface test runs a real local Node HTTP server applying the same rule and checks Chromium sends `Sec-Fetch-Dest: document` for a top-level visit and `iframe` inside a cross-site frame. The server rule itself is covered by the Rust HTTP test `a_restricted_stores_browser_created_orders_only_open_inside_a_frame`.
- **Alternatives considered:** Drive a real monokulo from the surface suite (it is fully mocked by design and has no backend).
- **Why:** The surface suite has no real backend; this still proves the one thing Rust tests can't: that a real browser sends the header the rule depends on.

### 16. No shared instance token on tenant creation
- **Step:** 8
- **Decision:** `POST /api/v1/admin/tenants` stays unauthenticated at the application layer; no shared instance token is added.
- **Alternatives considered:** Require the engine's existing instance admin token (monokulo's `engine.admin_token` setting, today only needed for the admin settings page) on tenant creation.
- **Why:** The engine now binds to loopback by default and warns loudly on a public bind (step 1), and has no public routes left, so the endpoint is reachable only by whatever can already reach the admin API. Requiring the instance token would make store creation fail on every install where monokulo's `engine.admin_token` isn't configured (it's optional today and empty by default), a new failure mode for little gain. Worth revisiting if the engine is ever deployed on a shared network.

### 17. The engine's per-IP limiter is removed; token-less routes use the admin limiter
- **Step:** 8
- **Decision:** `AppState::rate_limiter`, `rate_limit_middleware` and the `server.rate_limit_per_ip_per_min` setting are deleted. Tenant creation and `/status` now sit behind `admin_rate_limit_middleware`, which keys a token-less request on its address (per-token budget, default 120/min).
- **Alternatives considered:** Keep the per-IP limiter for those two routes.
- **Why:** Its job was the public routes. Monokulo is now the only caller, from one address, so a per-IP limit would only ever throttle monokulo as a whole; the admin limiter already falls back to the address and keeps a cap on both routes. Monokulo caches `/status`.

### 18. Old engine API callers sending `allowed_origins` are not refused; the CLI flag is
- **Step:** 8
- **Decision:** The engine's create/patch request types no longer have `allowed_origins`; serde ignores unknown fields, so a request still carrying one succeeds and the value is dropped. The `--bootstrap-wallet --allowed-origins` CLI flag is removed and now errors as an unrecognized argument (`scripts/dev-run.sh` updated). Migration `0014_drop_tenant_allowed_origins.sql` drops the column with a plain `DROP COLUMN` (no index/constraint uses it, same as 0005/0006/0013). `crates/scanner/src/http/public.rs` becomes `orders.rs` holding only the admin order creation.
- **Alternatives considered:** `deny_unknown_fields` on the requests; keep the CLI flag as a silently ignored no-op.
- **Why:** Silently accepting an ignored JSON field keeps any remaining script working (the field never did anything monokulo needs); an operator typing a CLI flag is better told it no longer exists than led to believe it did something.
