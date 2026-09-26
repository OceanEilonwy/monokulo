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
