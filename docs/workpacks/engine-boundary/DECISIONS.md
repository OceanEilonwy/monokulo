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
