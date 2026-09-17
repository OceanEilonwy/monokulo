<?php
/**
 * `WC_Gateway_Monokulo`: the WooCommerce-facing shell for Monokulo.
 *
 * @package Monokulo
 */

if ( ! defined( 'ABSPATH' ) ) {
	exit;
}

/**
 * Registers "Monero (via Monokulo)" as a WooCommerce checkout option
 * and, since WBS 1.5.2, actually hands off to the real engine at checkout.
 *
 * WBS 1.5.1 built registration only (the gateway shows up, disabled by
 * default - still true today, see the constructor's own comment on
 * `$this->enabled`). WBS 1.5.2 added `process_payment()`: a real
 * `POST /api/v1/t/{pk}/orders` call to the engine, keyed by the two settings
 * fields (`endpoint`/`public_key`) added in that same step as a documented,
 * manual stand-in for WBS 1.5.3's real one-click connect flow (see
 * `$api_base_url`'s own doc comment for the full reasoning and the field-
 * naming decision). This gateway still ships disabled by default - not a
 * placeholder left half-finished, but WBS 1.5.1's own stated acceptance
 * outcome, matching `docs/WOOCOMMERCE_ROADMAP.md` Stage 8: a merchant has to
 * actually connect a wallet (today: paste in `endpoint`/`public_key` by
 * hand; eventually: WBS 1.5.3's button) before this gateway can honor a real
 * checkout, and `is_available()` (below) additionally keeps an enabled-but-
 * unconfigured gateway off the checkout too, for the same reason.
 *
 * Every real decision - order creation, payment matching, exchange rates,
 * webhook delivery - lives in the Rust engine (`scanner`) and the
 * control plane sitting in front of it, per this repo's Stage 7 framing
 * ("PHP is a thin adapter, not a second implementation of anything"). This
 * class's job stops at being a faithful WooCommerce citizen: register
 * correctly, expose the right settings fields, and make one outbound HTTP
 * call per order. Nothing more belongs here by design - in particular, this
 * gateway never decides an order is paid (see `process_payment()`'s own doc
 * comment on why `payment_complete()` is never called here) and never
 * verifies a payment itself (WBS 1.5.4's webhook receiver's job).
 */
class WC_Gateway_Monokulo extends WC_Payment_Gateway {

	/**
	 * The one, fixed address of Monokulo's own hosted control plane -
	 * the service `control-plane/src/http/connect.rs` implements, sitting in
	 * front of (potentially many) real engines. **Not** the same thing as
	 * `$api_base_url`/`endpoint` below, and the two must never be conflated:
	 * `$api_base_url` is *an engine's* base URL (there can be many - every
	 * tenant's own hosted engine, or a self-hoster's own instance); this
	 * constant is *the* control plane's base URL, and for the hosted
	 * "Monokulo" product this plugin exists for
	 * (`docs/WOOCOMMERCE_ROADMAP.md`'s framing throughout, e.g. "we (not each
	 * individual merchant) run the infrastructure") there is exactly one of
	 * them - a fixed, known address, never something a merchant types in.
	 * That asymmetry is exactly why this is a class constant and
	 * `$api_base_url` is a per-merchant settings field: a merchant has to
	 * tell this plugin *which engine* their store talks to (today: pasted
	 * in by hand; via this step: written automatically by the connect flow
	 * below), but never *which control plane* - there's only the one this
	 * plugin ships already knowing about, exactly the way a Stripe or
	 * WooCommerce Payments extension hardcodes Stripe's/WooCommerce's own
	 * API host rather than asking the merchant to paste it in.
	 *
	 * **This is a placeholder, deliberately, not a real address**: as of
	 * this step, Monokulo's control plane has no real, decided
	 * production domain yet (checked `docs/WOOCOMMERCE_ROADMAP.md` and
	 * `work_notes.md` directly - neither names one). `cloud.monokulo.example`
	 * is lifted verbatim from the roadmap doc's own illustrative URL (Stage
	 * 6) specifically *because* it's already an obvious non-address -
	 * `.example` is the IANA-reserved TLD that can never resolve to anything
	 * real (RFC 2606) - rather than inventing a new placeholder that could
	 * later be mistaken for a real one. Whoever picks the real production
	 * domain only has to change this one line (and, ideally, drop the filter
	 * override below instead of leaving both).
	 *
	 * @var string
	 */
	const CONTROL_PLANE_BASE_URL = 'https://cloud.monokulo.example';

	/**
	 * The `platform` path segment this plugin identifies itself as to the
	 * control plane's generic, platform-agnostic connect flow
	 * (`GET /connect/{platform}` etc., see `connect.rs`'s own module doc
	 * comment) - `"woocommerce"`, matching exactly what `control-plane`'s own
	 * tests and `mock-woocommerce`'s driver already use for this platform, so
	 * every `store_connections.platform` row this plugin ever produces is
	 * consistent with what the mock already exercised end to end.
	 */
	const CONNECT_PLATFORM = 'woocommerce';

	/**
	 * The `admin-post.php` action name this gateway registers as `return_url`
	 * for the connect flow - see `get_connect_return_url()`'s own doc comment
	 * for why `admin-post.php`, specifically, is the callback mechanism this
	 * step chose.
	 */
	const CONNECT_RETURN_ACTION = 'monokulo_connect_return';

	/**
	 * The order-meta key `process_payment()` writes the engine's own
	 * `payment_id` under, and `find_order_by_payment_id()` (WBS 1.5.4) reads
	 * back to map an incoming webhook to a `WC_Order` - promoted to a
	 * constant, rather than the literal string 1.5.2 originally hardcoded
	 * inline, specifically because this step adds a second, independent call
	 * site that must agree with the first byte-for-byte: a silent drift
	 * between "what `process_payment()` writes" and "what the webhook
	 * receiver queries for" would make every real webhook fail to find its
	 * order, with no error anywhere near either call site to explain why.
	 *
	 * @var string
	 */
	const META_PAYMENT_ID = '_monokulo_payment_id';

	/**
	 * The order-meta key `event_already_applied()`/`mark_event_applied()`
	 * (WBS 1.5.4) use to record which webhook `event_id`s have already been
	 * applied to a given order - a JSON-encoded array of `evt_...` strings,
	 * not a comma-joined one, purely so `event_id`s (which this codebase
	 * never puts commas in, but nothing guarantees that structurally) can
	 * never be mis-split. See `event_already_applied()`'s own doc comment for
	 * why this exists at all.
	 *
	 * @var string
	 */
	const META_APPLIED_EVENT_IDS = '_monokulo_applied_webhook_event_ids';

	/**
	 * The `X-Monokulo-Signature` header, as PHP exposes it in `$_SERVER`:
	 * every incoming HTTP header `Name-Like-This` is normalized to
	 * `HTTP_NAME_LIKE_THIS` by PHP's own SAPI before user code ever sees it
	 * (a PHP/CGI convention, not a WordPress one) - named here as a constant
	 * so `handle_webhook()`'s one read of it and this doc comment's own
	 * explanation stay next to each other rather than the transform being
	 * re-derived silently at the call site.
	 *
	 * @var string
	 */
	const WEBHOOK_SIGNATURE_SERVER_KEY = 'HTTP_X_MONOKULO_SIGNATURE';

	/**
	 * Maps an engine `OrderStatus` (`src/status.rs::OrderStatus`, read
	 * directly - the seven-variant, authoritative enum, not assumed
	 * exhaustive from docs) to the WooCommerce order status this gateway
	 * transitions an order to when that engine status is announced by an
	 * `order.<status>` webhook event. `paid`/`overpaid` are deliberately
	 * **absent** from this table - see `apply_order_status_event()`'s own
	 * doc comment for why those two go through `WC_Order::payment_complete()`
	 * instead of a plain table lookup.
	 *
	 * Each row's reasoning (why *this* WooCommerce status, not some other
	 * plausible one) is documented in full on `apply_order_status_event()`,
	 * not repeated here - this table is the answer, that method's doc
	 * comment is the "why".
	 *
	 * @var array<string, string>
	 */
	const STATUS_MAP = array(
		'pending'     => 'pending',
		'unconfirmed' => 'on-hold',
		'confirming'  => 'on-hold',
		'partial'     => 'on-hold',
		'expired'     => 'cancelled',
	);

	/**
	 * The engine instance this store talks to, e.g. `https://pay.example.com`
	 * - no trailing slash (stripped in the getters below, so a merchant
	 * pasting one in doesn't produce a double slash in the outbound URL).
	 *
	 * **Why this exists at all in WBS 1.5.2, and not just at 1.5.3**:
	 * `process_payment()` (below) has to call *some* engine, at *some*
	 * tenant's public key, the moment this step lands - but the real
	 * one-click "Connect your Monero wallet" flow that would normally
	 * populate these two fields for real (mirroring `mock-woocommerce`'s
	 * `run_connect_flow`/`ConnectedCredentials`) is WBS 1.5.3's job, not
	 * built here. Rather than block 1.5.2 on 1.5.3, or fake the HTTP call
	 * behind a config constant nothing else in this plugin's own settings UI
	 * could reach, this step adds the two plain, manually-entered text
	 * fields (`endpoint`/`public_key` below) that are genuinely the minimum
	 * `process_payment()` needs to make a real call - explicitly documented
	 * here, in `init_form_fields()`, and in the settings screen itself as a
	 * stand-in for 1.5.3's real connect flow, not a finished credentials UX.
	 *
	 * **Field naming, chosen so 1.5.3 never has to rename these**: matched
	 * to `mock-woocommerce/src/lib.rs`'s own `ConnectedCredentials` struct
	 * (`public_key`, `endpoint`) - the exact two fields a real connect flow
	 * will have in hand once it exists, since that struct already mirrors
	 * `control-plane/src/http/connect.rs`'s real `FinishResponse`. Once WBS
	 * 1.5.3 exists, its callback handler can `update_option()` these same
	 * two settings keys directly (`$this->update_option( 'endpoint', ... )`)
	 * instead of introducing a second, differently-named pair of settings
	 * this class would then have to reconcile.
	 *
	 * Deliberately **not** named after `sk_` (the tenant's secret key) or
	 * given any field for one at all: `POST /api/v1/t/{pk}/orders` is a
	 * public endpoint (verified directly against `src/http/public.rs` and
	 * its route registration in `src/http/mod.rs` before writing this) -
	 * order creation only ever needs the tenant's *public* key in the URL
	 * path, never a secret token. A secret-key field belongs to whatever
	 * step first needs authenticated admin-API access from this plugin
	 * (refunds, webhook management, etc.) - not this one.
	 *
	 * @var string
	 */
	private $api_base_url;

	/**
	 * The tenant's public key (`pk_...`) this gateway creates orders under -
	 * see `$api_base_url`'s doc comment immediately above for why this is a
	 * plain manually-entered field at this step, and why no secret key
	 * field exists alongside it.
	 *
	 * @var string
	 */
	private $tenant_public_key;

	/**
	 * The tenant's secret key (`sk_...`), as of this step (WBS 1.5.3)
	 * genuinely returned by a real connect flow's `/finish` call
	 * (`FinishResponse::secret_token`, `control-plane/src/http/connect.rs`)
	 * and now actually stored - unlike WBS 1.5.2's deliberate choice not to
	 * add a secret-key field at all, back when nothing in this plugin ever
	 * saw one.
	 *
	 * **Why store it now when nothing here reads it yet**: discarding it
	 * would be actively harmful, not just unused - `consume_connect_token`
	 * redeems the connect token exactly once (see that method's own
	 * atomicity doc comment), so a `secret_token` that isn't saved the one
	 * time it's ever handed over is *gone* until the merchant reconnects
	 * their wallet from scratch. A future step that needs authenticated
	 * admin-API access (webhook management, refunds, anything behind
	 * `AuthedTenant`) would otherwise have to force every already-connected
	 * merchant through the whole browser-redirect connect flow again just to
	 * get back a value this plugin already had in hand once and threw away.
	 * Storing a string nobody reads yet costs nothing; losing a single-use
	 * secret costs a real support burden later. Exposed as a real (masked,
	 * `type => 'password'`) settings field below for the same reason
	 * `endpoint`/`public_key` are - so a self-hoster or an advanced merchant
	 * can also paste one in by hand, exactly like those two fields already
	 * allow - not because this step has any code path that reads it back
	 * out.
	 *
	 * @var string
	 */
	private $secret_token;

	/**
	 * The webhook's own signing secret (`whsec_...`), returned by
	 * `call_connect_finish()`'s `/finish` response and saved (since WBS
	 * 1.5.3) under the `webhook_signing_secret` option key - see that
	 * method's own doc comment for why it's saved outside `$this->form_fields`
	 * entirely. Read here, in this step, for the first time: it's the HMAC
	 * key `verify_webhook_signature()` below checks every incoming webhook
	 * delivery against.
	 *
	 * @var string
	 */
	private $webhook_signing_secret;

	/**
	 * Sets up the gateway's identity and settings fields.
	 *
	 * WooCommerce instantiates every class registered via the
	 * `woocommerce_payment_gateways` filter (see `monokulo.php`)
	 * with no constructor arguments, so every property this gateway needs
	 * has to be derivable here with no external input beyond WordPress's
	 * own options table (via `get_option()`, populated by
	 * `init_settings()` below) - there is no other injection point at
	 * this stage of WooCommerce's own lifecycle.
	 */
	public function __construct() {
		// `id` is the gateway's permanent identifier: it's the array key
		// WooCommerce's settings are stored under
		// (`woocommerce_monokulo_settings` in `wp_options`), the
		// value that ends up as `$order->get_payment_method()` on every
		// order this gateway processes, and the suffix of the webhook
		// endpoint WBS 1.5.4 registers (`woocommerce_api_{$this->id}`).
		// Changing it later would silently orphan every existing order's
		// payment-method record and any already-registered webhook URL, so
		// it's treated as fixed from this first step onward, not a cosmetic
		// label. Chosen to read as "the id", not a version-agnostic English
		// phrase like "monero" (which a future unrelated Monero gateway
		// could just as reasonably claim) - `monokulo` names the
		// actual product per `docs/WOOCOMMERCE_ROADMAP.md`'s own framing of
		// the hosted service by that name throughout.
		$this->id = 'monokulo';

		// No custom checkout icon yet - out of scope for a registration-only
		// step, and WooCommerce renders gateways with no `icon` perfectly
		// normally (an empty string is this property's own documented
		// default), so there's nothing to work around here.
		$this->icon = '';

		// This gateway never renders its own fields inline at WooCommerce's
		// checkout - per `docs/WOOCOMMERCE_ROADMAP.md` Stage 7's explicit
		// integration choice, paying with Monero means redirecting the
		// customer to the engine's own already-built, already-tested
		// `/pay/v1/{pk}/{payment_id}` checkout page, not embedding a new
		// widget into WooCommerce's checkout form. `has_fields = false` is
		// what tells WooCommerce's checkout template not to reserve any
		// inline space for this gateway.
		$this->has_fields = false;

		// `method_title`/`method_description` are what a merchant sees on
		// WooCommerce's own "Payments" settings list (Settings > Payments) -
		// distinct from `title`/`description` below, which is what the
		// *customer* sees at checkout. Conflating the two is a common
		// mistake in WooCommerce gateway plugins; kept deliberately separate
		// here even though nothing yet enforces the distinction functionally,
		// because getting it right from the first version avoids a later
		// "why does the merchant admin screen say something meant for
		// customers" bug report.
		$this->method_title       = __( 'Monokulo', 'monokulo' );
		$this->method_description = __(
			'Accept Monero at checkout without ever holding customer funds or your own spend key - Monokulo watches the chain for you and only ever needs a view key. Connect your wallet to enable this gateway for real.',
			'monokulo'
		);

		// `supports` is WooCommerce's own mechanism for a gateway to declare
		// which of its many optional checkout/order features it implements
		// (refunds, subscriptions, pre-orders, tokenization, etc.) -
		// WooCommerce's core checkout and admin-order-screen code both
		// consult this array before offering a feature (e.g. it won't show
		// a "Refund" button for a gateway that hasn't declared `refunds`),
		// rather than assuming every gateway implements everything.
		// `'products'` is the baseline every ordinary one-time-payment
		// gateway declares (WooCommerce's own bundled gateways all include
		// it) - it means "this gateway works for a cart containing ordinary
		// products", as opposed to gateways that only make sense for
		// recurring subscriptions. Nothing else is declared here
		// deliberately: refunds specifically would be a false claim right
		// now (there is no refund flow) and no other `supports` value is
		// developed as of WBS 1.5.1's own scope.
		$this->supports = array( 'products' );

		// Populates `$this->form_fields`, the schema `init_settings()` (next)
		// reads defaults from and WooCommerce's own settings-page renderer
		// walks to draw the actual HTML form.
		$this->init_form_fields();

		// Loads whatever's actually been saved in `wp_options` into
		// `$this->settings`, falling back to each field's own `default` from
		// `init_form_fields()` above for anything never saved yet (e.g. a
		// brand new install). This has to run *after* `init_form_fields()`
		// - there'd be nothing to load defaults from otherwise - and
		// *before* the `get_option()` calls immediately following it, which
		// read out of the `$this->settings` array this call populates.
		$this->init_settings();

		// `title`/`description` are the customer-facing checkout strings -
		// pulled from saved settings when present, otherwise the same
		// literal WBS 1.5.1's own acceptance outcome names verbatim
		// ("Monero (via Monokulo)"), so a fresh install already shows
		// the right label with zero configuration, while still letting a
		// merchant override the wording later without a code change.
		$this->title       = $this->get_option( 'title', __( 'Monero (via Monokulo)', 'monokulo' ) );
		$this->description = $this->get_option(
			'description',
			__( 'Pay with Monero. You will be redirected to a secure Monokulo payment page to complete your purchase.', 'monokulo' )
		);

		// The 1.5.3-stand-in connection fields (see `$api_base_url`'s own doc
		// comment above for why these two specific fields exist already,
		// manually-entered, at this step). `trim()`, not just a raw
		// `get_option()` read: a merchant pasting a value from a dashboard
		// notoriously picks up leading/trailing whitespace, and an
		// un-trimmed `pk_...\n` would fail `create_engine_order()`'s outbound
		// call in a way that looks identical to a genuinely wrong key -
		// exactly the kind of support ticket worth avoiding for free.
		$this->api_base_url     = trim( (string) $this->get_option( 'endpoint', '' ) );
		$this->tenant_public_key = trim( (string) $this->get_option( 'public_key', '' ) );
		$this->secret_token      = trim( (string) $this->get_option( 'secret_token', '' ) );
		$this->webhook_signing_secret = trim( (string) $this->get_option( 'webhook_signing_secret', '' ) );

		// No explicit `$this->enabled = $this->get_option( 'enabled' )` line
		// here, deliberately - checked directly against WooCommerce's own
		// source (`WC_Payment_Gateway::init_settings()`,
		// includes/abstracts/abstract-wc-payment-gateway.php) rather than
		// assumed: that method, called just above, already sets
		// `$this->enabled` itself from `$this->settings['enabled']`
		// (`'yes' === $this->settings['enabled'] ? 'yes' : 'no'`) - a second
		// assignment here would be pure duplication, not a safety net. This
		// matches the pattern WooCommerce's own bundled gateways use (e.g.
		// `WC_Gateway_BACS`'s constructor never touches `$this->enabled`
		// either, for the identical reason - confirmed by reading
		// `includes/gateways/bacs/class-wc-gateway-bacs.php` directly).
		//
		// What actually makes this gateway disabled by default is the
		// `enabled` field's own `'default' => 'no'` in `init_form_fields()`
		// below, which `init_settings()` falls back to for a fresh install
		// with nothing saved yet - and that default is not an oversight this
		// step left for later, it is WBS 1.5.1's own stated outcome
		// ("disabled state is fine at this step"). A gateway with no
		// connected wallet has no `pk_`/`sk_`/`endpoint` to call yet (that
		// only exists once WBS 1.5.3's connect flow runs), so registering it
		// pre-enabled would let a merchant offer a payment method that WBS
		// 1.5.2's `process_payment()` cannot yet honor - exactly the failure
		// mode a disabled-by-default gateway avoids.

		// The standard WooCommerce hook a gateway registers so that
		// submitting its own settings form (Settings > Payments > this
		// gateway) actually persists - WooCommerce fires
		// `woocommerce_update_options_payment_gateways_{$this->id}` on save,
		// and `process_admin_options()` (inherited from `WC_Payment_Gateway`,
		// not overridden here - its default behavior of validating and
		// saving `$this->form_fields` against submitted POST data is exactly
		// right for this step's plain checkbox/text fields) is what actually
		// writes the new values back to `wp_options`. Without this line, the
		// settings form would render correctly but silently discard every
		// change on submit.
		add_action( 'woocommerce_update_options_payment_gateways_' . $this->id, array( $this, 'process_admin_options' ) );

		// The real connect-flow callback (WBS 1.5.3) - see
		// `get_connect_return_url()`'s own doc comment for why
		// `admin_post_{action}` specifically is the right WordPress
		// mechanism for this, and `handle_connect_return()`'s for why
		// registering it here in the constructor is safe (identical
		// reasoning to the `woocommerce_update_options_...` registration
		// immediately above: this constructor only ever runs once
		// WooCommerce's own `WC_Payment_Gateways::init()` has already fired,
		// which happens on every request including the one `admin-post.php`
		// itself bootstraps).
		add_action( 'admin_post_' . self::CONNECT_RETURN_ACTION, array( $this, 'handle_connect_return' ) );

		// The real webhook receiver (WBS 1.5.4) - WooCommerce's own real,
		// documented mechanism for a plugin's webhook endpoint, confirmed
		// directly against the *installed* WooCommerce source before relying
		// on it (not just its doc comments): `WC()->api_request_url( $id )`
		// (already used by `get_webhook_receiver_url()` above, since 1.5.3)
		// builds a URL carrying a `wc-api={id}` query var (or `/wc-api/{id}/`
		// under pretty permalinks); `src/Internal/Utilities/
		// LegacyRestApiStub.php::maybe_process_wc_api_query_var()` (hooked on
		// `parse_request`, read directly) is what actually dispatches that
		// var, firing `do_action( 'woocommerce_api_' . $api_request )` for
		// *any* site that has this hook registered - regardless of whether
		// the separate, full "WooCommerce Legacy REST API" extension is
		// installed. That file's own doc comment explains why the stub
		// exists at all: the versioned `/wc-api/v1-3/...` REST endpoints
		// were genuinely removed from core in WC 9.0, but the plain
		// `woocommerce_api_{id}` gateway-callback mechanism (what PayPal's
		// own legacy IPN handler still uses too, per
		// `wc-deprecated-functions.php::woocommerce_legacy_paypal_ipn()`,
		// read directly) was deliberately kept working without it - this is
		// real, current, installed-source-confirmed behavior on this
		// environment's WooCommerce 11.1.0, not an assumption carried over
		// from an older WooCommerce version where the whole legacy API was
		// still in core wholesale.
		add_action( 'woocommerce_api_' . $this->id, array( $this, 'handle_webhook' ) );

		// Flashes a one-line success/error notice on this gateway's own
		// settings screen after a connect attempt redirects back to it - see
		// `maybe_render_connect_notice()`'s own doc comment for why a plain
		// query-string flag is used here rather than `WC_Admin_Settings::
		// add_error()` (which only works within the same request that
		// renders the settings form, and the connect callback is a separate
		// request).
		add_action( 'admin_notices', array( $this, 'maybe_render_connect_notice' ) );
	}

	/**
	 * Defines the settings fields WooCommerce renders on this gateway's own
	 * settings screen (Settings > Payments > Monokulo) and that
	 * `init_settings()`/`get_option()` above read defaults from.
	 *
	 * Kept to the minimum this step actually needs, not a placeholder set of
	 * every field a *later* step will eventually want (an endpoint override,
	 * a webhook secret) - those belong to whichever step actually uses them.
	 * Building empty settings fields now, with nothing yet reading or
	 * writing them for real, would be exactly the kind of unrequested scope
	 * this project's own established practice (see `work_notes.md`'s
	 * progress log) has repeatedly flagged and avoided elsewhere.
	 *
	 * **WBS 1.5.3 note**: `endpoint`/`public_key` (added at 1.5.2, as a
	 * manual stand-in for this step) are kept, unrenamed - see this class's
	 * own `$api_base_url` doc comment for why the real connect flow below
	 * writes into these exact same two keys rather than introducing a
	 * second pair. `secret_token` is new (see `$secret_token`'s own doc
	 * comment for why it's stored at all, given nothing reads it back yet).
	 * No `webhook_signing_secret` field here, deliberately: unlike the three
	 * fields above, a merchant has no legitimate way to independently know
	 * that value by hand - it's minted fresh by the engine only when a
	 * webhook is registered (`connect.rs::finish`), so it's only ever
	 * written programmatically (`process_connect_return()` below, via a
	 * plain `update_option()` call outside `$this->form_fields` entirely -
	 * confirmed directly against `WC_Settings_API::update_option()`
	 * /`get_option()`, both read/write `$this->settings[$key]` by string key
	 * with no dependency on the key being declared here), never rendered as
	 * an editable field a merchant could accidentally paste a wrong value
	 * into.
	 */
	public function init_form_fields() {
		$this->form_fields = array(
			'enabled'     => array(
				'title'   => __( 'Enable/Disable', 'monokulo' ),
				'type'    => 'checkbox',
				'label'   => __( 'Enable Monero payments via Monokulo', 'monokulo' ),
				// See the constructor's own comment on `$this->enabled` -
				// this default is what makes "disabled state is fine at
				// this step" the actual out-of-the-box behavior, not just
				// documentation of an intention. WBS 1.5.3's own connect
				// callback (`process_connect_return()` below) is now a real
				// second way this flips to `'yes'` - see that method's own
				// doc comment for why flipping it there, not just leaving it
				// for the merchant to toggle afterward, is the WBS's own
				// stated outcome, not an extra liberty this step is taking.
				'default' => 'no',
			),
			'title'       => array(
				'title'       => __( 'Title', 'monokulo' ),
				'type'        => 'text',
				'description' => __( 'The label the customer sees for this payment method at checkout.', 'monokulo' ),
				'default'     => __( 'Monero (via Monokulo)', 'monokulo' ),
				'desc_tip'    => true,
			),
			'description' => array(
				'title'       => __( 'Description', 'monokulo' ),
				'type'        => 'textarea',
				'description' => __( 'The explanatory text the customer sees for this payment method at checkout.', 'monokulo' ),
				'default'     => __( 'Pay with Monero. You will be redirected to a secure Monokulo payment page to complete your purchase.', 'monokulo' ),
				'desc_tip'    => true,
			),

			'connection'  => array(
				'title'       => __( 'Connection', 'monokulo' ),
				'type'        => 'title',
				'description' => __( 'Click below to connect your Monero wallet through Monokulo - this fills in everything below automatically and enables this gateway. Advanced/self-hosted users can also enter these by hand instead.', 'monokulo' ),
			),

			// The real WBS 1.5.3 button - see `generate_monokulo_connect_html()`'s
			// own doc comment for exactly what it renders and why this needs
			// a genuinely custom field type rather than reusing `'title'`
			// (whose own `generate_title_html()` renders fixed markup this
			// step needs to deviate from: a real `<a href=...>` built fresh
			// on every render, not static text).
			'connect'     => array(
				'type'        => 'monokulo_connect',
				'description' => __( 'You will be sent to Monokulo to sign in (or sign up) and confirm the connection, then returned here automatically.', 'monokulo' ),
			),

			'endpoint'    => array(
				'title'       => __( 'Engine API base URL', 'monokulo' ),
				'type'        => 'text',
				'description' => __( 'The base URL of the Monokulo engine this store talks to (no trailing slash needed). Filled in automatically by Connect above - only edit this by hand for a self-hosted engine.', 'monokulo' ),
				'default'     => '',
				'placeholder' => 'https://pay.example.com',
				'desc_tip'    => true,
			),
			'public_key'  => array(
				'title'       => __( 'Tenant public key', 'monokulo' ),
				'type'        => 'text',
				'description' => __( 'Your Monokulo tenant\'s public key (starts with pk_). Filled in automatically by Connect above.', 'monokulo' ),
				'default'     => '',
				'placeholder' => 'pk_...',
				'desc_tip'    => true,
			),
			'secret_token' => array(
				'title'       => __( 'Tenant secret key', 'monokulo' ),
				'type'        => 'password',
				'description' => __( 'Your Monokulo tenant\'s secret key (starts with sk_). Filled in automatically by Connect above. Not currently used by this plugin for anything - kept so a future update never has to ask you to reconnect just to retrieve it again.', 'monokulo' ),
				'default'     => '',
				'placeholder' => 'sk_...',
				'desc_tip'    => true,
			),
		);
	}

	/**
	 * Extends `WC_Payment_Gateway::is_available()` (checked directly,
	 * `includes/abstracts/abstract-wc-payment-gateway.php` - the base
	 * implementation only checks `$this->enabled === 'yes'` plus a currency
	 * restriction) with the one additional fact genuinely true of *this*
	 * gateway: an enabled gateway with no engine URL or no public key
	 * configured cannot honor a real checkout at all -
	 * `create_engine_order()` below would just throw on the first customer
	 * to try it. Excluding it from `get_available_payment_gateways()` in
	 * that state is the same "don't offer what you can't honor" principle
	 * WBS 1.5.1 already used to justify shipping disabled by default (see
	 * that constructor's own comment on `$this->enabled`) - this is that
	 * same principle applied to the one new failure mode 1.5.2 introduces.
	 *
	 * @return bool
	 */
	public function is_available() {
		if ( ! parent::is_available() ) {
			return false;
		}

		return $this->has_credentials();
	}

	/**
	 * Whether this gateway has enough to actually create an order right now
	 * - factored out of `is_available()` so `generate_monokulo_connect_html()`
	 * below can show "Connected"/"Reconnect" status using the exact same
	 * fact, rather than a second, potentially-drifting check. Deliberately
	 * does **not** consider `secret_token`/`webhook_signing_secret` - see
	 * `$secret_token`'s own doc comment: order creation (the only thing
	 * `is_available()` is gating) never needs either.
	 *
	 * @return bool
	 */
	private function has_credentials() {
		return '' !== $this->api_base_url && '' !== $this->tenant_public_key;
	}

	/**
	 * WooCommerce calls this - and only this - when a customer places an
	 * order with this gateway selected. Checked directly against
	 * WooCommerce's own real source, not assumed from memory:
	 *
	 * - `WC_Payment_Gateway::process_payment( $order_id )`'s own doc comment
	 *   (`includes/abstracts/abstract-wc-payment-gateway.php`) states the
	 *   contract in full: "When implemented, this should return the success
	 *   and redirect in an array", e.g.
	 *   `array( 'result' => 'success', 'redirect' => $url )`.
	 * - `WC_Gateway_BACS::process_payment()` and `WC_Gateway_COD::
	 *   process_payment()` (`includes/gateways/{bacs,cod}/class-wc-gateway-
	 *   {bacs,cod}.php`) are WooCommerce's own reference implementations of
	 *   that contract - both return exactly
	 *   `array( 'result' => 'success', 'redirect' => $this->get_return_url( $order ) )`.
	 *   This gateway deliberately does **not** call `get_return_url()`
	 *   (WooCommerce's own thank-you page) the way those two do - both of
	 *   them consider the order paid the instant `process_payment()` runs,
	 *   which is never true here. `redirect` only has to be a URL
	 *   WooCommerce's checkout JS will send the browser to, not specifically
	 *   the thank-you page - confirmed by reading `WC_Checkout::
	 *   process_order_payment()` (`includes/class-wc-checkout.php`) directly:
	 *   it reads `$result['redirect']` verbatim, either via `wp_redirect()`
	 *   (classic, non-AJAX submit) or as the `redirect` field of the JSON
	 *   `wp_send_json( $result )` sends back (AJAX/Blocks checkout) - neither
	 *   path constrains the URL's shape or origin. So `redirect` here is the
	 *   engine's own already-built checkout page,
	 *   `GET /pay/v1/{pk}/{payment_id}`, off-site by design.
	 * - Failure is signaled by *throwing*, not by returning
	 *   `array( 'result' => 'fail' )` - confirmed against `WC_Checkout::
	 *   process_checkout()` directly: it calls `process_order_payment()`
	 *   (and therefore this method) from inside its own top-level
	 *   `try { ... } catch ( Exception $e ) { wc_add_notice( $e->getMessage(),
	 *   'error' ); }` block, so a thrown `Exception` becomes exactly the
	 *   customer-facing checkout error WooCommerce already knows how to
	 *   render - no bundled gateway happens to demonstrate this path (BACS/
	 *   COD never fail), but it is the real, source-confirmed mechanism this
	 *   method relies on for every failure branch below.
	 *
	 * Deliberately does **not** call `$order->payment_complete()` or change
	 * the order's status - the order WooCommerce just created is already
	 * `pending`/awaiting payment, and it should stay exactly that until a
	 * real payment is actually observed on-chain. Marking it paid here,
	 * before the customer has even reached the engine's own checkout page,
	 * would be simply false. That transition is WBS 1.5.4's job (the webhook
	 * receiver + status mapping), not this step's - this method's only job
	 * is "hand off to the engine, and tell WooCommerce where to send the
	 * customer next."
	 *
	 * @param int $order_id Order ID.
	 * @return array{result: string, redirect: string}
	 * @throws Exception If the order can't be loaded, this gateway isn't
	 *                    configured, or the engine's order-creation call
	 *                    fails or returns something unexpected - see
	 *                    `WC_Checkout::process_checkout()`'s own catch block
	 *                    above for what WooCommerce does with it.
	 */
	public function process_payment( $order_id ) {
		$order = wc_get_order( $order_id );
		if ( ! $order instanceof WC_Order ) {
			throw new Exception( __( 'Could not load this order to start the Monero payment.', 'monokulo' ) );
		}

		$engine_order = $this->create_engine_order( $order );

		// The one point in this gateway's whole flow that ever sees the
		// mapping between a WC order and the engine's own payment_id -
		// recorded now, while it's in hand, so it isn't thrown away.
		// WBS 1.5.4's webhook receiver will need exactly this lookup (an
		// incoming delivery carries a payment_id, not a WC order id) to know
		// which order to update; nothing here *consumes* that meta key yet -
		// that consumption is 1.5.4's job, not built here.
		$order->update_meta_data( self::META_PAYMENT_ID, $engine_order['payment_id'] );
		$order->add_order_note(
			sprintf(
				/* translators: %s: Monokulo payment_id */
				__( 'Customer redirected to Monokulo checkout for payment_id %s.', 'monokulo' ),
				$engine_order['payment_id']
			)
		);
		$order->save();

		return array(
			'result'   => 'success',
			'redirect' => $this->get_engine_checkout_url( $engine_order['payment_id'] ),
		);
	}

	/**
	 * Calls the engine's real, public `POST /api/v1/t/{pk}/orders` endpoint
	 * (`src/http/public.rs::create_order`, read directly rather than
	 * paraphrased - a **public** route: no secret token, only the tenant's
	 * public key in the URL path) to create a real order for `$order`, and
	 * returns the decoded JSON response
	 * (`payment_id`/`address`/`xmr_amount_piconero`/`fiat_amount`/
	 * `fiat_currency`/`expires_at`) on success.
	 *
	 * Every failure mode below throws a plain, customer-safe `Exception`
	 * (never the raw engine/HTTP error text - that's logged instead, via
	 * `self::log()`, for the merchant to actually diagnose) - see
	 * `process_payment()`'s own doc comment for why throwing, specifically,
	 * is this contract's real failure signal.
	 *
	 * @param WC_Order $order The order to create an engine-side order for.
	 * @return array{payment_id: string, address: string, xmr_amount_piconero: int,
	 *               fiat_amount: string, fiat_currency: string, expires_at: int}
	 * @throws Exception See above.
	 */
	private function create_engine_order( WC_Order $order ) {
		if ( '' === $this->api_base_url || '' === $this->tenant_public_key ) {
			// Reachable even though `is_available()` should normally have
			// kept an unconfigured gateway off the checkout entirely - e.g.
			// an already-rendered checkout page submitted after an admin
			// disables/unconfigures the gateway in another tab. Not purely
			// defensive dead code: this exact path is what this plugin's own
			// mocked-HTTP unit test exercises to prove the unconfigured case
			// fails loudly rather than silently calling `wp_remote_post()`
			// with an empty URL.
			throw new Exception(
				__( 'Monero payments are not fully configured for this store yet. Please contact the store owner.', 'monokulo' )
			);
		}

		$request_url = $this->get_orders_endpoint_url();

		// `fiat_amount` as a plain "123.45"-shaped decimal string, never a
		// locale-formatted one (no thousands separator, always a `.` decimal
		// point) - matches exactly what the engine's own
		// `exchange_rate::compute_xmr_amount()` parses
		// (`src/exchange_rate.rs`, read directly): at most two decimal
		// places, no separators, no scientific notation. `number_format()`
		// with an explicit `.`/`''` pair guarantees this regardless of the
		// site's own locale settings, which `(string) $order->get_total()`
		// alone would not.
		$body = array(
			'fiat_amount'       => number_format( (float) $order->get_total(), 2, '.', '' ),
			'fiat_currency'     => $order->get_currency(),
			// The order's own numeric id, not `get_order_number()` - the
			// latter is filterable (some plugins prefix it, e.g. "WC-1042")
			// and only ever meant for human display, whereas this value
			// exists purely so a merchant can cross-reference an engine-side
			// order against `wp_posts`/`wc_orders` later; a stable raw id
			// serves that better than a display string liable to change
			// under a filter this gateway doesn't control.
			'merchant_order_id' => (string) $order->get_id(),
			'description'       => sprintf(
				/* translators: 1: order number, 2: site name */
				__( 'Order #%1$s on %2$s', 'monokulo' ),
				$order->get_order_number(),
				get_bloginfo( 'name' )
			),
		);

		$response = wp_remote_post(
			$request_url,
			array(
				'headers' => array( 'Content-Type' => 'application/json' ),
				'body'    => wp_json_encode( $body ),
				// Order creation itself needs no live Monero daemon
				// connectivity (only a registered tenant + a configured
				// exchange rate - see `src/http/public.rs::create_order`),
				// so this is a fast, ordinary HTTP round trip; 30s is
				// generous headroom for a loaded engine or a slow network
				// hop, not a tuned value.
				'timeout' => 30,
			)
		);

		if ( is_wp_error( $response ) ) {
			$this->log(
				sprintf( 'Order creation request to %s failed: %s', $request_url, $response->get_error_message() ),
				'error'
			);
			throw new Exception(
				__( 'Could not reach Monokulo to start this payment. Please try again shortly.', 'monokulo' )
			);
		}

		$status_code = (int) wp_remote_retrieve_response_code( $response );
		$raw_body    = wp_remote_retrieve_body( $response );

		if ( 200 !== $status_code ) {
			$this->log(
				sprintf( 'Order creation request to %s returned HTTP %d: %s', $request_url, $status_code, $raw_body ),
				'error'
			);
			throw new Exception(
				__( 'Monokulo could not start this payment. Please contact the store or try again.', 'monokulo' )
			);
		}

		$decoded = json_decode( $raw_body, true );
		if ( ! is_array( $decoded ) || empty( $decoded['payment_id'] ) ) {
			$this->log(
				sprintf( 'Order creation response from %s was not the expected shape: %s', $request_url, $raw_body ),
				'error'
			);
			throw new Exception(
				__( 'Monokulo returned an unexpected response. Please contact the store.', 'monokulo' )
			);
		}

		return $decoded;
	}

	/**
	 * `{endpoint}/api/v1/t/{pk}/orders` - the engine's real, public
	 * order-creation route (`src/http/mod.rs`'s own `.route(...)` table,
	 * read directly). `rtrim()` on the base URL, not on the merchant's
	 * stored option: an admin pasting a trailing slash into the settings
	 * field is exactly the kind of copy-paste artifact worth absorbing here
	 * rather than rejecting at save time.
	 *
	 * @return string
	 */
	private function get_orders_endpoint_url() {
		return sprintf(
			'%s/api/v1/t/%s/orders',
			rtrim( $this->api_base_url, '/' ),
			rawurlencode( $this->tenant_public_key )
		);
	}

	/**
	 * `{endpoint}/pay/v1/{pk}/{payment_id}` - the engine's own already-built
	 * checkout page (`src/http/mod.rs`'s `.route("/pay/v1/{pk}/{payment_id}",
	 * get(public::payment_page))`, read directly) this gateway redirects the
	 * customer to. `rawurlencode()` on `payment_id` even though the engine's
	 * own ids are UUID-shaped today (never containing characters this would
	 * change) - cheap, correct-by-construction defense against that
	 * assumption quietly becoming false in a later engine version, rather
	 * than this gateway silently relying on it.
	 *
	 * @param string $payment_id The engine's own order id, from
	 *                            `create_engine_order()`'s response.
	 * @return string
	 */
	private function get_engine_checkout_url( $payment_id ) {
		return sprintf(
			'%s/pay/v1/%s/%s',
			rtrim( $this->api_base_url, '/' ),
			rawurlencode( $this->tenant_public_key ),
			rawurlencode( $payment_id )
		);
	}

	/**
	 * The control plane's own base URL, run through
	 * `monokulo_control_plane_base_url` - a filter, not just the bare
	 * constant, for two concrete reasons: it's what lets this plugin's own
	 * tests point `process_connect_return()`/`generate_monokulo_connect_html()`
	 * at a fake control plane (`pre_http_request` alone can't substitute for
	 * the *browser-facing* `GET /connect/...` URL a test never actually
	 * fetches, only asserts the shape of - so the base URL itself has to be
	 * swappable, not just the HTTP call), and it's the one real, sanctioned
	 * escape hatch for anyone running their own control plane instance
	 * (a self-hoster who wants the one-click flow against infrastructure
	 * they run themselves, rather than ours) without forking this file.
	 *
	 * @return string
	 */
	private function get_control_plane_base_url() {
		return apply_filters( 'monokulo_control_plane_base_url', self::CONTROL_PLANE_BASE_URL );
	}

	/**
	 * The WordPress mechanism chosen for `return_url` - the URL the control
	 * plane's browser redirect (step 3 of `connect.rs`'s own module doc
	 * comment) has to land on, on *this* site, in a shape this plugin can
	 * recognize and handle.
	 *
	 * Reasoned from WordPress's own documented conventions for exactly this
	 * shape of problem ("an external service redirects the browser back into
	 * a plugin's own admin screen, plugin does some server-side work, then
	 * sends the browser on"), not invented fresh - three real WordPress
	 * mechanisms fit:
	 *
	 * 1. **A dedicated `admin-post.php?action=...` handler** (chosen here).
	 *    WordPress core's own documented pattern for "a link or button
	 *    triggers a server-side action, which redirects somewhere when
	 *    done" - `wp-admin/admin-post.php` fires `do_action(
	 *    "admin_post_{$_GET['action']}" )` for a logged-in user (the exact
	 *    case here - the merchant is coming back into a wp-admin session
	 *    they were already in when they clicked "Connect"), runs inside the
	 *    ordinary wp-admin bootstrap (so `current_user_can()`, `admin_url()`,
	 *    `wp_safe_redirect()` all just work with no extra setup), and needs
	 *    no new URL-routing surface beyond a plain `add_action()` call - the
	 *    same shape this class already uses for
	 *    `woocommerce_update_options_payment_gateways_{$this->id}`.
	 * 2. **A query var checked on this gateway's own settings-page render.**
	 *    Rejected: it would mean the connect flow's actual credential-saving
	 *    side effect happens as an incidental part of *rendering a page*
	 *    (`generate_monokulo_connect_html()`/`admin_options()`), which is
	 *    the wrong place for a state-changing action to live - a page render
	 *    can be triggered more than once (a refresh, a prefetch) in ways a
	 *    dedicated action handler isn't, and mixing "display the form" with
	 *    "consume a single-use token" is exactly the kind of GET-with-side-
	 *    effects WordPress's own admin-notices/admin-post split exists to
	 *    avoid.
	 * 3. **A REST API route** (`register_rest_route`). A legitimate
	 *    alternative real WooCommerce/WordPress extensions do use for this -
	 *    rejected only because it's strictly more machinery than this step
	 *    needs (a new namespace, permission callback, route registration)
	 *    for a URL that only ever has to be recognized by *this* site's own
	 *    logged-in browser session, which `admin-post.php` already handles
	 *    with a single `add_action()` call.
	 *
	 * This is also exactly the shape real "Connect" style WooCommerce
	 * gateway plugins are documented to use for their own OAuth-return
	 * handlers (Stripe's and PayPal's own WooCommerce extensions both route
	 * their connect callbacks through an `admin-post.php` action rather than
	 * a REST route or a settings-page query var) - reasoned here from
	 * WordPress's own documented `admin-post.php` mechanism directly, per
	 * this step's own brief, since this environment has no way to fetch
	 * their source live to confirm byte-for-byte.
	 *
	 * @return string
	 */
	private function get_connect_return_url() {
		return admin_url( 'admin-post.php?action=' . self::CONNECT_RETURN_ACTION );
	}

	/**
	 * The transient key this gateway's own single-use connect nonce is
	 * stored under between `generate_monokulo_connect_html()` (which mints
	 * it) and `process_connect_return()` (which consumes it exactly once).
	 * A WordPress transient, not a plain `option`: it's meant to expire on
	 * its own (`10 * MINUTE_IN_SECONDS`, chosen to match `connect.rs`'s own
	 * `CONNECT_TOKEN_TTL_SECONDS` exactly - the nonce only ever needs to
	 * survive the same round trip the connect token itself does, so there's
	 * no reason for it to outlive that token), which is precisely what
	 * `set_transient()`'s own expiry argument is for and a plain `option`
	 * has no built-in equivalent of.
	 *
	 * @return string
	 */
	private function connect_nonce_transient_key() {
		return 'monokulo_connect_nonce_' . $this->id;
	}

	/**
	 * Builds the real `GET {control_plane_base_url}/connect/{platform}?...`
	 * URL (step 1 of `connect.rs`'s own module doc comment) this gateway
	 * sends the merchant's browser to.
	 *
	 * @param string $nonce A freshly generated, single-use nonce - the
	 *                       caller is responsible for also having stored it
	 *                       (`connect_nonce_transient_key()`) before handing
	 *                       this URL out, so `process_connect_return()` has
	 *                       something real to check the redirect-back
	 *                       `nonce` against.
	 * @return string
	 */
	private function build_connect_start_url( $nonce ) {
		return sprintf(
			'%s/connect/%s?site_url=%s&return_url=%s&nonce=%s',
			rtrim( $this->get_control_plane_base_url(), '/' ),
			rawurlencode( self::CONNECT_PLATFORM ),
			rawurlencode( home_url( '/' ) ),
			rawurlencode( $this->get_connect_return_url() ),
			rawurlencode( $nonce )
		);
	}

	/**
	 * Renders the "Connect your Monero wallet" settings-screen field - a
	 * genuinely custom `WC_Settings_API` field type (`'monokulo_connect'`,
	 * dispatched to this method by `generate_settings_html()`'s own
	 * `method_exists( $this, 'generate_' . $type . '_html' )` check, read
	 * directly against `abstract-wc-settings-api.php` before writing this),
	 * not a reuse of the built-in `'title'` type: `generate_title_html()`
	 * renders fixed, static markup from `$data['title']`/`$data['description']`
	 * alone, but this field has to build a real `<a href>` fresh on every
	 * render (a new nonce, a new transient, a URL that depends on whether a
	 * wallet is already connected) - none of which a static type could do.
	 *
	 * Mints a fresh nonce **on every render of this field**, deliberately
	 * not in the constructor (which every gateway instantiation on every
	 * front-end/admin request also runs, via `WC_Payment_Gateways::init()`
	 * on `woocommerce_init` - minting and overwriting the one live nonce on
	 * every unrelated page load would routinely invalidate an
	 * already-in-flight connect attempt before the merchant even got back).
	 * This method only ever runs when this gateway's own settings screen is
	 * actually being rendered (`WC_Settings_Page::output()` calling
	 * `$gateway->admin_options()` for the matching `section`), which is
	 * exactly the point a fresh nonce is actually needed.
	 *
	 * @param string $key  Field key (`'connect'`).
	 * @param array  $data Field data from `init_form_fields()`.
	 * @return string
	 */
	public function generate_monokulo_connect_html( $key, $data ) {
		$field_key = $this->get_field_key( $key );

		// Same default-filling `generate_text_html()` itself does (read
		// directly) before ever touching `$data['desc_tip']`/
		// `$data['description']` - `init_form_fields()`'s own `'connect'`
		// entry only sets `type`/`description`, and `get_description_html()`
		// below indexes `$data['desc_tip']` unconditionally, which would be
		// an undefined-array-key warning on every real render without this.
		$data = wp_parse_args(
			$data,
			array(
				'desc_tip'    => false,
				'description' => '',
			)
		);

		$nonce = bin2hex( random_bytes( 16 ) );
		set_transient( $this->connect_nonce_transient_key(), $nonce, 10 * MINUTE_IN_SECONDS );
		$connect_url = $this->build_connect_start_url( $nonce );

		ob_start();
		?>
		<tr valign="top">
			<th scope="row" class="titledesc">
				<label for="<?php echo esc_attr( $field_key ); ?>"><?php esc_html_e( 'Connect your Monero wallet', 'monokulo' ); ?></label>
			</th>
			<td class="forminp">
				<?php if ( $this->has_credentials() ) : ?>
					<p>
						<?php esc_html_e( 'Connected as', 'monokulo' ); ?>
						<code><?php echo esc_html( $this->tenant_public_key ); ?></code>
					</p>
				<?php endif; ?>
				<a href="<?php echo esc_url( $connect_url ); ?>" id="<?php echo esc_attr( $field_key ); ?>" class="button button-primary">
					<?php
					echo $this->has_credentials()
						? esc_html__( 'Reconnect your Monero wallet', 'monokulo' )
						: esc_html__( 'Connect your Monero wallet', 'monokulo' );
					?>
				</a>
				<?php echo $this->get_description_html( $data ); // WPCS: XSS ok. ?>
			</td>
		</tr>
		<?php
		return ob_get_clean();
	}

	/**
	 * `'connect'`'s own `type` (`monokulo_connect`) has no real form input
	 * for `process_admin_options()` to read back on save - it only ever
	 * renders a link. Without this method, `get_field_value()` would fall
	 * through to `validate_text_field()` on a `null` POST value (checked
	 * directly, `abstract-wc-settings-api.php::get_field_value()`), which is
	 * harmless but pointless (nothing should ever read a
	 * `monokulo_connect` settings key) and, on PHP 8.1+, a `null`-to-string
	 * coercion warning waiting to happen the moment that fallback path's own
	 * implementation changes. Returning `''` directly is the explicit,
	 * intentional no-op this field type actually needs - the *real* save
	 * path for this field's effect is `process_connect_return()` below, not
	 * WooCommerce's ordinary settings-form POST at all.
	 *
	 * @param string $key   Field key.
	 * @param mixed  $value Posted value (always unused here).
	 * @return string Always `''`.
	 */
	public function validate_monokulo_connect_field( $key, $value ) {
		return '';
	}

	/**
	 * The real `admin_post_{action}` handler WordPress core dispatches to
	 * (see `get_connect_return_url()`'s own doc comment for why
	 * `admin-post.php` is the mechanism at all). Deliberately just two
	 * lines: everything that can actually be tested without WordPress's own
	 * `exit` getting in the way lives in `process_connect_return()` below,
	 * which this method calls and then acts on. `wp_safe_redirect()` +
	 * `exit` immediately after is WordPress core's own documented way to end
	 * an `admin-post.php` handler - `exit` specifically because a handler
	 * that fell through to WordPress's own further output after redirecting
	 * would corrupt the response, not because of anything specific to this
	 * plugin.
	 *
	 * The `current_user_can( 'manage_woocommerce' )` guard exists because
	 * `admin_post_{action}` (unlike this gateway's own settings screen,
	 * which WooCommerce's own menu registration already restricts to that
	 * capability) fires for **any** logged-in user, regardless of role -
	 * defense in depth on top of the nonce check in
	 * `process_connect_return()`, which is this leg's real CSRF defense
	 * (see that method's own doc comment), against a logged-in
	 * low-privilege account (e.g. a customer with a WordPress login) hitting
	 * this URL directly rather than a genuine admin who initiated the flow.
	 */
	public function handle_connect_return() {
		if ( ! current_user_can( 'manage_woocommerce' ) ) {
			wp_die( esc_html__( 'You do not have permission to do this.', 'monokulo' ), '', array( 'response' => 403 ) );
		}

		$redirect_url = $this->process_connect_return( wp_unslash( $_GET ) ); // phpcs:ignore WordPress.Security.NonceVerification.Recommended
		wp_safe_redirect( $redirect_url );
		exit;
	}

	/**
	 * The real logic behind steps 3-4 of `connect.rs`'s own module doc
	 * comment: the merchant's browser has just landed back on this site
	 * carrying `token`/`nonce` query params, and this method has to decide
	 * whether to trust them, redeem the token for real credentials, and (on
	 * success) save them and enable the gateway - the WBS's own stated
	 * outcome for this step ("the gateway becomes enabled"), not left for
	 * the merchant to separately toggle afterward.
	 *
	 * **Split out of `handle_connect_return()` above specifically so this is
	 * directly unit-testable**: no `wp_safe_redirect()`/`exit` in here, just
	 * a pure(-ish) function of "the query args a request carried" ->
	 * "the URL to send the browser to next" (plus the real side effect of
	 * saving settings on success) - `tests/ConnectFlowTest.php` calls this
	 * method directly, the same way `tests/ProcessPaymentTest.php` calls
	 * `process_payment()` directly rather than driving a real HTTP request
	 * through WordPress's own router.
	 *
	 * **The nonce check - the actual CSRF defense this whole leg has**:
	 * `return_url` is a public, guessable URL (it's this exact method's own
	 * `admin-post.php` address); anyone could, in principle, cause a
	 * logged-in merchant's browser to `GET` it with an arbitrary `token`.
	 * What they can't do is know the *nonce* this plugin itself generated
	 * and stored server-side in `connect_nonce_transient_key()` at the
	 * moment the *real* connect attempt started - so a mismatched, missing,
	 * or already-consumed (the transient is deleted immediately, win or
	 * lose - single use, not single check) nonce means this request did not
	 * originate from a connect flow this plugin itself kicked off, and is
	 * rejected before ever calling `/finish` at all, exactly per this
	 * step's own brief ("nonce mismatch must be rejected, not silently
	 * accepted"). `hash_equals()`, not `===`, for the same constant-time
	 * reasoning `docs/WOOCOMMERCE_WBS.md` already calls out for WBS 1.5.4's
	 * webhook signature check - a nonce is shorter-lived and lower-value
	 * than an HMAC key, but there's no real cost to using the safe
	 * comparison here too, and no argument for `===` being the least bit
	 * simpler.
	 *
	 * **On a failed `/finish` call**: no setting is touched at all - an
	 * already-connected merchant whose *reconnect* attempt fails (expired
	 * token, control plane hiccup, etc.) keeps whatever credentials they had
	 * before, rather than this method clobbering good settings with a
	 * failure.
	 *
	 * @param array $query_args The request's GET query parameters (`token`,
	 *                            `nonce`), already `wp_unslash()`ed by the
	 *                            caller - accepted as a plain array, not read
	 *                            from `$_GET` directly, purely so a test can
	 *                            hand this method an arbitrary array without
	 *                            needing to fake superglobals.
	 * @return string The URL to send the merchant's browser to next - always
	 *                 this gateway's own settings screen, with a
	 *                 `monokulo_connected` or
	 *                 `monokulo_connect_error` flag appended for
	 *                 `maybe_render_connect_notice()` below to read.
	 */
	public function process_connect_return( array $query_args ) {
		$token = isset( $query_args['token'] ) ? sanitize_text_field( $query_args['token'] ) : '';
		$nonce = isset( $query_args['nonce'] ) ? sanitize_text_field( $query_args['nonce'] ) : '';

		$stored_nonce = get_transient( $this->connect_nonce_transient_key() );
		// Single-use regardless of outcome, per this method's own doc
		// comment - a stale or already-checked nonce must never be
		// re-checkable.
		delete_transient( $this->connect_nonce_transient_key() );

		if ( '' === $token || '' === $nonce || false === $stored_nonce || ! hash_equals( (string) $stored_nonce, $nonce ) ) {
			$this->log( 'Connect return rejected: missing token, or the nonce did not match what this site generated.', 'warning' );
			return $this->build_settings_url( array( 'monokulo_connect_error' => 'nonce' ) );
		}

		$finish = $this->call_connect_finish( $token );
		if ( null === $finish ) {
			return $this->build_settings_url( array( 'monokulo_connect_error' => 'finish' ) );
		}

		$this->update_option( 'endpoint', $finish['endpoint'] );
		$this->update_option( 'public_key', $finish['public_key'] );
		$this->update_option( 'secret_token', $finish['secret_token'] );
		if ( ! empty( $finish['webhook_signing_secret'] ) ) {
			// Deliberately not a declared `form_fields` entry - see
			// `init_form_fields()`'s own comment on why. `update_option()`
			// (checked directly, `WC_Settings_API::update_option()`) writes
			// into `$this->settings[$key]` by plain string key regardless of
			// whether that key was ever declared, so this is a real,
			// persisted write, not a no-op.
			$this->update_option( 'webhook_signing_secret', $finish['webhook_signing_secret'] );
		}
		// The WBS's own stated outcome for this step: connecting a wallet
		// enables the gateway outright, not just fills in fields for the
		// merchant to separately flip a switch on afterward.
		$this->update_option( 'enabled', 'yes' );

		return $this->build_settings_url( array( 'monokulo_connected' => '1' ) );
	}

	/**
	 * `POST {control_plane_base_url}/connect/{platform}/finish` (step 4-5 of
	 * `connect.rs`'s own module doc comment / its `finish` handler, read
	 * directly): redeems the single-use connect token for real credentials.
	 *
	 * **`webhook_url` is sent, deliberately, even though this plugin has no
	 * webhook-receiving route yet** (that's WBS 1.5.4's job entirely - see
	 * `get_webhook_receiver_url()`'s own doc comment). Chosen over omitting
	 * it, for a reason parallel to why `$secret_token` is stored now despite
	 * nothing reading it yet: `mock-woocommerce::run_connect_flow` - "1.4.2's
	 * already-proven logic" this step's own brief explicitly says to mirror
	 * - always registers a webhook as part of the very same connect flow
	 * (confirmed directly, `mock-woocommerce/src/lib.rs::run_connect_flow_with`,
	 * not assumed); there is no "connect without a webhook" variant of the
	 * already-proven protocol to mirror instead. Registering now, pointed at
	 * a URL nothing yet answers, means WBS 1.5.4 only has to *build the
	 * receiver* - not also force every merchant who already connected under
	 * 1.5.3 back through the whole browser-redirect flow a second time just
	 * to register a webhook that could have been registered the first time
	 * for free. This is safe to do before 1.5.4 exists: checked directly
	 * against `src/http/admin.rs::create_webhook` - registration only
	 * validates the URL's scheme (`http`/`https`), never reachability (SSRF
	 * validation is explicitly deferred to delivery time, per that
	 * handler's own comment), so a webhook pointed at a route that
	 * doesn't respond yet fails to *register* for. Deliveries against it
	 * will simply fail (and retry, per the engine's own delivery-worker
	 * policy) until 1.5.4 lands - an acceptable, self-healing gap for a
	 * feature whose receiver is being built in the very next step of the
	 * same project, not a real production outage window.
	 *
	 * Returns `null` on any failure (transport error, non-200, or a response
	 * missing any of the three fields this plugin actually needs) - this
	 * method never throws, unlike `create_engine_order()`: this isn't inside
	 * WooCommerce's own checkout `try`/`catch`, so there's no framework
	 * mechanism here to catch an `Exception` and turn it into a user-facing
	 * message; the caller (`process_connect_return()`) is responsible for
	 * turning a `null` into the right redirect + notice instead.
	 *
	 * @param string $token The single-use connect token from the `return_url`
	 *                       redirect's own `token` query param.
	 * @return array{public_key: string, secret_token: string, endpoint: string,
	 *               webhook_signing_secret?: string}|null
	 */
	private function call_connect_finish( $token ) {
		$url = rtrim( $this->get_control_plane_base_url(), '/' ) . '/connect/' . self::CONNECT_PLATFORM . '/finish';

		$response = wp_remote_post(
			$url,
			array(
				'headers' => array( 'Content-Type' => 'application/json' ),
				'body'    => wp_json_encode(
					array(
						'token'       => $token,
						'webhook_url' => $this->get_webhook_receiver_url(),
					)
				),
				// Same generous, untuned headroom `create_engine_order()`
				// above uses, for the same reason: an ordinary HTTP round
				// trip with no reason to be slow, not a value chosen from
				// measurement.
				'timeout' => 30,
			)
		);

		if ( is_wp_error( $response ) ) {
			$this->log( sprintf( 'Connect finish request to %s failed: %s', $url, $response->get_error_message() ), 'error' );
			return null;
		}

		$status_code = (int) wp_remote_retrieve_response_code( $response );
		$raw_body    = wp_remote_retrieve_body( $response );

		// A bare 401 is the real, documented shape of *every* failure mode
		// `connect.rs::finish` can produce (unknown/expired/already-consumed
		// token, a rejected webhook URL, an internal error) - collapsed
		// deliberately on that side for enumeration-defense reasons (see
		// `finish`'s own doc comment); this side has nothing more specific
		// to recover from any of them, so every non-200 is treated
		// identically here too.
		if ( 200 !== $status_code ) {
			$this->log( sprintf( 'Connect finish request to %s returned HTTP %d: %s', $url, $status_code, $raw_body ), 'error' );
			return null;
		}

		$decoded = json_decode( $raw_body, true );
		if ( ! is_array( $decoded ) || empty( $decoded['public_key'] ) || empty( $decoded['secret_token'] ) || empty( $decoded['endpoint'] ) ) {
			$this->log( sprintf( 'Connect finish response from %s was not the expected shape: %s', $url, $raw_body ), 'error' );
			return null;
		}

		return $decoded;
	}

	/**
	 * The webhook URL this plugin's own (not-yet-built, WBS 1.5.4) receiver
	 * will eventually answer on - `WC()->api_request_url( $this->id )`,
	 * WooCommerce's own real, documented mechanism for a plugin's
	 * `woocommerce_api_{id}` endpoint (confirmed directly,
	 * `class-woocommerce.php::api_request_url()`, before relying on it - it
	 * resolves to `{site}/wc-api/{id}/` with pretty permalinks or
	 * `{site}/?wc-api={id}` without, never anything this plugin has to
	 * construct by hand). Used only by `call_connect_finish()` above - see
	 * that method's own doc comment for why registering it now, before
	 * 1.5.4 builds anything to answer it, is deliberate and safe.
	 *
	 * @return string
	 */
	private function get_webhook_receiver_url() {
		return WC()->api_request_url( $this->id );
	}

	/**
	 * The real `woocommerce_api_{id}` hook target (see the constructor's own
	 * comment on why this specific mechanism, checked directly against
	 * WooCommerce's installed source) - WordPress core dispatches to this
	 * for every incoming delivery to `get_webhook_receiver_url()`'s own URL.
	 *
	 * Kept to the same "two lines, all real logic elsewhere" shape
	 * `handle_connect_return()` above already established, for the identical
	 * reason: `process_webhook_request()` below is directly unit-testable
	 * (no PHP superglobals, no `exit`), which this thin wrapper is not - see
	 * `tests/WebhookReceiverTest.php`'s own class doc comment for how it
	 * drives this hook's real logic without going through an actual HTTP
	 * request.
	 *
	 * Reads the request body via `file_get_contents( 'php://input' )` -
	 * **never** `$_POST` - because the signature this method hands to
	 * `process_webhook_request()` has to be verified against the *exact raw
	 * bytes* the engine signed (`shared/src/webhook_sign.rs::sign_payload`
	 * hashes `payload: &[u8]` as sent, read directly). `$_POST` doesn't even
	 * apply here (the engine sends a raw JSON body, not
	 * `application/x-www-form-urlencoded`/multipart form fields, so PHP
	 * never populates `$_POST` for this request at all) - but even if it
	 * did, going through it would mean verifying a signature against a
	 * *re-serialized* body PHP itself reconstructed, which can differ from
	 * the original byte-for-byte (key order, whitespace, numeric formatting)
	 * even when it decodes to the same logical JSON - exactly the "even
	 * whitespace-identical-looking JSON can byte-differ after a decode/
	 * re-encode round trip" failure mode this step's own brief calls out by
	 * name. `php://input` is the one source that can't have been touched by
	 * anything in between.
	 *
	 * The signature header is read from `$_SERVER` (see
	 * `WEBHOOK_SIGNATURE_SERVER_KEY`'s own doc comment for the
	 * `X-Monokulo-Signature` -> `HTTP_X_MONOKULO_SIGNATURE` transform) and
	 * `wp_unslash()`-ed - the same treatment `handle_connect_return()` above
	 * already gives `$_GET`, for the identical reason: WordPress's own
	 * `wp_magic_quotes()` (`wp-includes/load.php`, read directly) backslash-
	 * escapes `$_SERVER` exactly like `$_GET`/`$_POST`/`$_COOKIE`, not just
	 * the superglobals an HTML form would populate.
	 *
	 * `status_header()`, not `wp_die()`: unlike `handle_connect_return()`'s
	 * `wp_die()` (whose HTML error page is for a *human* who followed a bad
	 * link), the only consumer of this response is the engine's own
	 * delivery worker, which reads nothing but the HTTP status code -
	 * `attempt_delivery()` in `src/webhook_delivery.rs` (read directly)
	 * decides `delivered: status.is_success()` and otherwise reschedules a
	 * retry with backoff, never inspecting the body at all. A bare status
	 * code is therefore the entire real contract this endpoint has to
	 * honor; `wp_die()`'s HTML-page machinery would be pure overhead no
	 * caller of this endpoint ever looks at.
	 */
	public function handle_webhook() {
		$raw_body  = (string) file_get_contents( 'php://input' );
		$signature = isset( $_SERVER[ self::WEBHOOK_SIGNATURE_SERVER_KEY ] )
			? (string) wp_unslash( $_SERVER[ self::WEBHOOK_SIGNATURE_SERVER_KEY ] ) // phpcs:ignore WordPress.Security.NonceVerification.Recommended, WordPress.Security.ValidatedSanitizedInput.MissingUnslash
			: '';

		status_header( $this->process_webhook_request( $raw_body, $signature ) );
		exit;
	}

	/**
	 * The real logic behind an incoming webhook delivery: verify, decode,
	 * find the order, dedupe, apply. Split out of `handle_webhook()` above
	 * for the same reason `process_connect_return()` is split out of
	 * `handle_connect_return()` - directly testable, no request
	 * superglobals or `exit` involved, a pure(-ish) function of "the raw
	 * body and header this request carried" -> "the HTTP status code to
	 * respond with" (plus the real side effect, on success, of updating the
	 * matched order).
	 *
	 * **The four response codes, and why each one specifically** (per this
	 * step's own brief: "decide the right status code... and document why"):
	 *
	 * - **200**: signature verified, payload well-formed, a matching order
	 *   was found, and the event was applied (or was already applied before
	 *   - see `event_already_applied()`'s own doc comment for why a
	 *   duplicate delivery is *also* a 200, not some other code). This is
	 *   the only status `run_delivery_tick` in `src/webhook_delivery.rs`
	 *   treats as `delivered: true` - anything else means the engine's
	 *   delivery worker will retry with backoff (`backoff_seconds()`, read
	 *   directly) until `max_attempts` is reached, per that file's own doc
	 *   comment on `run_delivery_tick`.
	 * - **401 Unauthorized**: signature missing or invalid. Mirrors this
	 *   *engine's own* convention for exactly this situation - `ApiError::
	 *   Unauthorized => (StatusCode::UNAUTHORIZED, ...)` in `src/http/
	 *   mod.rs`, read directly - rather than inventing a different
	 *   convention on the PHP side for what is, semantically, the identical
	 *   fact ("this request did not prove it knows the shared secret").
	 *   **No order lookup or mutation ever happens before this check passes**
	 *   - the whole point of verifying first is that everything downstream
	 *   can trust `payment_id` came from the engine, not from anyone who
	 *   found this URL.
	 * - **400 Bad Request**: the signature is valid but the body isn't the
	 *   envelope shape `enqueue_webhook_event()` in `src/scanner.rs` always
	 *   produces (`event`/`event_id`/`payment_id`, read directly - every
	 *   event this engine ever sends has all three). A signed-but-malformed
	 *   body is a client error on the sender's side, not a "we don't
	 *   recognize this order" case (404) or an auth failure (401) - 400 is
	 *   the one of the three that actually means "your request, not our
	 *   data, is the problem," matching ordinary REST convention.
	 * - **404 Not Found**: a well-formed, correctly-signed event for a
	 *   `payment_id` with no matching order *on this WordPress site*.
	 *   Mirrors the engine's own `ApiError::NotFound => StatusCode::
	 *   NOT_FOUND` convention for "the referenced resource doesn't exist
	 *   here" (`src/http/mod.rs`, same file as above) - chosen deliberately
	 *   over a 2xx "swallow it silently" response, because an unknown
	 *   `payment_id` reaching a *correctly-signed* request (it already
	 *   passed the 401 check, so this webhook's `signing_secret` really is
	 *   this site's) is a real operational fact worth an operator noticing
	 *   in logs - a stale webhook registration surviving a database reset,
	 *   or (in principle) a site's webhook secret having leaked - not
	 *   something to hide by pretending success. Also deliberately not
	 *   treated as a reason to retry forever: since `process_payment()`
	 *   always creates the engine-side order and records its `payment_id`
	 *   (`META_PAYMENT_ID`) *before* that order can possibly generate any
	 *   webhook event at all, "the order doesn't exist yet, try again
	 *   later" is not a real race this endpoint has to accommodate - an
	 *   unknown `payment_id` here means "never will," not "not yet."
	 *
	 * @param string $raw_body  The exact raw request body bytes, unmodified.
	 * @param string $signature The `X-Monokulo-Signature` header value, or
	 *                            `''` if the header was absent.
	 * @return int The HTTP status code to respond with.
	 */
	public function process_webhook_request( $raw_body, $signature ) {
		if ( ! $this->verify_webhook_signature( $raw_body, $signature ) ) {
			$this->log( 'Webhook rejected: missing or invalid X-Monokulo-Signature.', 'warning' );
			return 401;
		}

		$event = json_decode( $raw_body, true );
		if ( ! is_array( $event )
			|| empty( $event['event'] ) || ! is_string( $event['event'] )
			|| empty( $event['event_id'] ) || ! is_string( $event['event_id'] )
			|| empty( $event['payment_id'] ) || ! is_string( $event['payment_id'] )
		) {
			$this->log( sprintf( 'Webhook rejected: correctly signed but not the expected envelope shape: %s', $raw_body ), 'error' );
			return 400;
		}

		$order = $this->find_order_by_payment_id( $event['payment_id'] );
		if ( ! $order instanceof WC_Order ) {
			$this->log(
				sprintf( 'Webhook for payment_id %s (event %s) has no matching order on this site.', $event['payment_id'], $event['event_id'] ),
				'warning'
			);
			return 404;
		}

		if ( $this->event_already_applied( $order, $event['event_id'] ) ) {
			// Already handled - see event_already_applied()'s own doc
			// comment for why this is still a 200, not a rejection.
			return 200;
		}

		$this->apply_webhook_event( $order, $event );
		$this->mark_event_applied( $order, $event['event_id'] );
		$order->save();

		return 200;
	}

	/**
	 * Verifies `X-Monokulo-Signature` against a freshly computed HMAC-
	 * SHA256 of `$raw_body`, keyed by this gateway's stored
	 * `webhook_signing_secret`.
	 *
	 * **`hash_equals()`, never `===`** - this step's own brief states the
	 * requirement explicitly, and the reasoning is worth restating here, in
	 * this codebase's own words, rather than only citing the rule: PHP's
	 * `===` string comparison (like Rust's plain `String`/`&str` equality)
	 * short-circuits at the first differing byte, so the *time* a rejection
	 * takes leaks *how many leading bytes were already correct* - the
	 * textbook byte-at-a-time forgery oracle. `shared/src/webhook_sign.rs::
	 * verify_signature`'s own doc comment (read directly) states this exact
	 * concern for the Rust side, which uses `Mac::verify_slice` (backed by
	 * `subtle`'s constant-time `ct_eq`) for the identical reason; `hash_
	 * equals()` is PHP's own standard-library equivalent - a comparison
	 * whose running time does not depend on where or whether the two inputs
	 * first differ, specifically documented (php.net) as existing for
	 * "comparing a string to a value the user is not supposed to know",
	 * which is exactly this situation (an attacker submitting a guessed
	 * signature against this public endpoint).
	 *
	 * **Compared as hex strings, not decoded to raw bytes first** - a
	 * deliberate difference from the Rust side, not an inconsistency:
	 * `verify_signature` in `shared/src/webhook_sign.rs` decodes the
	 * presented hex to raw tag bytes before `verify_slice`, because Rust's
	 * standard library has no built-in constant-time string comparison to
	 * reach for directly. PHP's `hash_equals()` *is* a constant-time byte
	 * comparison, and works identically whether the two strings being
	 * compared happen to be hex text or raw bytes - comparing the lowercase
	 * hex `hash_hmac()` already returns is the direct, standard, officially
	 * documented PHP idiom for verifying an HMAC (the `hash_equals()` manual
	 * page's own canonical example does exactly this), so there is no
	 * decode-first step to add here that would buy anything Rust's own
	 * decode-then-compare doesn't already get for free out of `hash_
	 * equals()`.
	 *
	 * Cross-checked against `shared/src/webhook_sign.rs`'s own fixed
	 * known-vector test constants in `tests/WebhookSignatureTest.php` - see
	 * that file for the exact secret/payload/signature triple this method
	 * must reproduce byte-for-byte.
	 *
	 * @param string $raw_body  The exact raw request body bytes.
	 * @param string $signature The presented `X-Monokulo-Signature` value.
	 * @return bool
	 */
	private function verify_webhook_signature( $raw_body, $signature ) {
		if ( '' === $this->webhook_signing_secret || '' === $signature ) {
			// No configured secret (gateway never connected, or an admin
			// blanked the field by hand) or no presented header - neither
			// is a "wrong" signature to log as a forgery attempt, just an
			// unauthenticated request; still rejected identically below via
			// hash_equals() returning false against an empty comparand, but
			// short-circuited here so hash_hmac() never runs against an
			// empty key for no reason.
			return false;
		}

		$expected = hash_hmac( 'sha256', $raw_body, $this->webhook_signing_secret );

		return hash_equals( $expected, $signature );
	}

	/**
	 * Finds the `WC_Order` an incoming webhook's `payment_id` refers to, by
	 * querying for the `META_PAYMENT_ID` meta key `process_payment()`
	 * writes - via `wc_get_orders()` (WooCommerce's own real order-query
	 * abstraction, `WC_Order_Query` under the hood), **not** a raw SQL query
	 * against `wp_postmeta`/the HPOS order-meta tables directly, because
	 * WooCommerce's order storage backend is itself pluggable (the legacy
	 * custom-post-type store, or High-Performance Order Storage's own
	 * dedicated tables) and only its own query API is guaranteed to work
	 * against whichever one a given site actually has active.
	 *
	 * **The exact `meta_key`/`meta_value` args used here, confirmed to work
	 * on *both* backends by reading the real installed source, not
	 * assumed**: `WC_Data_Store_WP::get_wp_query_args()`
	 * (`includes/data-stores/class-wc-data-store-wp.php`) passes any
	 * unrecognized top-level query var - `meta_key`/`meta_value` included -
	 * straight through to `WP_Query` verbatim, which is what the legacy
	 * CPT store's `query()` ultimately runs; that store's own `query()`
	 * (`includes/data-stores/class-wc-order-data-store-cpt.php`) explicitly
	 * flags a *`meta_query`*-shaped arg (the array form) as `$unsupported_
	 * args` and fires a `doing_it_wrong()` notice for it - so the array form
	 * would have been the wrong choice here even though it also happens to
	 * work on HPOS. On the HPOS side, `OrdersTableQuery::` (`src/Internal/
	 * DataStores/Orders/OrdersTableQuery.php`) builds its own internal
	 * meta-query mechanism from exactly this same top-level `meta_key`/
	 * `meta_value`/`meta_compare` "shortcut" shape (read directly - it
	 * folds `$this->args['meta_key']`/`['meta_value']` into a `$shortcut_
	 * meta_query` before handing off to `OrdersTableMetaQuery`), the same
	 * WP_Query-style convention the legacy store also understands. The
	 * plain `meta_key`/`meta_value` pair used below is therefore the one
	 * shape genuinely portable across both backends - not a guess, and not
	 * the `meta_query` array shape WooCommerce's own CPT store explicitly
	 * warns is CPT-unsupported.
	 *
	 * @param string $payment_id The engine's own order id from the webhook
	 *                            payload.
	 * @return WC_Order|null
	 */
	private function find_order_by_payment_id( $payment_id ) {
		$orders = wc_get_orders(
			array(
				'meta_key'   => self::META_PAYMENT_ID, // phpcs:ignore WordPress.DB.SlowDBQuery.slow_db_query_meta_key
				'meta_value' => $payment_id, // phpcs:ignore WordPress.DB.SlowDBQuery.slow_db_query_meta_value
				'limit'      => 1,
				'return'     => 'objects',
			)
		);

		return isset( $orders[0] ) && $orders[0] instanceof WC_Order ? $orders[0] : null;
	}

	/**
	 * Whether `$event_id` has already been applied to `$order` -
	 * `META_APPLIED_EVENT_IDS` order meta, a JSON-encoded array checked with
	 * `in_array()`.
	 *
	 * **Why this exists at all**: `run_delivery_tick` in `src/webhook_
	 * delivery.rs` (read directly) retries a delivery on anything short of
	 * a 2xx response, with exponential backoff, up to `max_attempts` - and,
	 * separately, any real HTTP delivery can also succeed on the engine's
	 * side while its response is lost in transit (a network blip between
	 * this server sending `200` and the engine receiving it), which the
	 * engine's own retry logic cannot distinguish from an outright failure.
	 * Either way, this receiver *will* see the same `event_id` more than
	 * once for a perfectly healthy delivery, not just a broken one - and
	 * `enqueue_webhook_event()` in `src/scanner.rs` (read directly) mints
	 * exactly one fresh `event_id` per real status *transition*, baking it
	 * into the stored payload once so every retry of that delivery resends
	 * the identical id under the identical signature. Skipping a
	 * already-seen `event_id` is what makes re-applying it a no-op instead
	 * of, for example, appending the same order note twice or re-running
	 * `payment_complete()`'s stock-reduction side effects a second time.
	 *
	 * @param WC_Order $order    The order to check.
	 * @param string   $event_id The webhook envelope's own `event_id`.
	 * @return bool
	 */
	private function event_already_applied( WC_Order $order, $event_id ) {
		$applied = json_decode( (string) $order->get_meta( self::META_APPLIED_EVENT_IDS ), true );
		return is_array( $applied ) && in_array( $event_id, $applied, true );
	}

	/**
	 * Records `$event_id` as applied to `$order` - see
	 * `event_already_applied()`'s own doc comment for why this exists.
	 * Appends to the existing list rather than replacing it (an order sees
	 * at most a handful of transitions over its real lifetime - the seven
	 * `order.<status>` events plus, rarely, the two double-spend ones - so
	 * unbounded growth is not a real concern here the way it might be for a
	 * key logging every request an endpoint ever received).
	 *
	 * Deliberately does **not** call `$order->save()` - the caller
	 * (`process_webhook_request()`) saves once, after every mutation this
	 * event implies has been applied to the in-memory order object,
	 * matching this class's existing pattern elsewhere (e.g.
	 * `process_payment()`'s own single `$order->save()` after both
	 * `update_meta_data()` and `add_order_note()`).
	 *
	 * @param WC_Order $order    The order to record against.
	 * @param string   $event_id The webhook envelope's own `event_id`.
	 */
	private function mark_event_applied( WC_Order $order, $event_id ) {
		$applied   = json_decode( (string) $order->get_meta( self::META_APPLIED_EVENT_IDS ), true );
		$applied   = is_array( $applied ) ? $applied : array();
		$applied[] = $event_id;
		$order->update_meta_data( self::META_APPLIED_EVENT_IDS, wp_json_encode( array_values( array_unique( $applied ) ) ) );
	}

	/**
	 * Applies one already-verified, already-deduped webhook event to
	 * `$order` - the real status-mapping/note-writing decision for every
	 * event type this engine actually sends.
	 *
	 * **The real event catalog, checked directly against every call site in
	 * `src/scanner.rs`** (grepped for `"order\.` rather than assumed from
	 * this step's own brief alone) - `enqueue_webhook_event()` is called
	 * from exactly two places: `recompute_and_notify_in_tx()` (one
	 * `order.<status>` event per actual status *transition*, for all seven
	 * `OrderStatus` variants - `src/status.rs`, read directly, confirmed
	 * exhaustive) and `void_and_notify()`/`unvoid_as_false_positive()`
	 * (the two double-spend events). Nine event types total; this method
	 * handles all nine, not just `order.paid`.
	 *
	 * **Why the two double-spend events never set a WC status themselves**:
	 * both `void_and_notify()` and `unvoid_as_false_positive()` (read
	 * directly, `src/scanner.rs`) call `recompute_and_notify_in_tx()` -
	 * which enqueues its own `order.<status>` event under its own fresh
	 * `event_id` whenever the status genuinely changed - *before* enqueueing
	 * their own `order.double_spend_detected`/`order.double_spend_reversed`
	 * event, in the same database transaction. That means any real status
	 * consequence of a void or an un-void is **already** announced,
	 * correctly, by its own paired `order.<status>` event - handled by
	 * `apply_order_status_event()` below like any other transition. A
	 * double-spend event forcing some *second*, independently-guessed
	 * status change here would either duplicate that (if the two events are
	 * both delivered and processed) or actively fight it (since delivery
	 * order between two independently-retried webhook rows is not
	 * guaranteed - this receiver could see `double_spend_reversed` before
	 * or after its paired `order.<status>` event). This is also exactly
	 * what makes "recomputed, not hardcoded back to processing regardless
	 * of amount" (this step's own brief, for `double_spend_reversed`
	 * specifically) true *for free*: the real recomputation already
	 * happened engine-side, against the engine's own authoritative ledger,
	 * and is delivered as an honest, independent event - this method's only
	 * job for both double-spend events is to make sure the merchant
	 * *notices* (a prominent order note), never to re-derive or second-guess
	 * a status this receiver has no authoritative data to recompute anyway
	 * (the double-spend payloads carry no amount/confirmation data at all -
	 * only `payment_id`, plus `txid` for the reversal).
	 *
	 * @param WC_Order $order The already-matched order.
	 * @param array    $event The decoded webhook envelope (`event`,
	 *                         `event_id`, `payment_id`, plus whatever
	 *                         event-specific fields that event type carries).
	 */
	private function apply_webhook_event( WC_Order $order, array $event ) {
		$event_type = $event['event'];

		$status_by_event_type = array(
			'order.pending'     => 'pending',
			'order.unconfirmed' => 'unconfirmed',
			'order.confirming'  => 'confirming',
			'order.paid'        => 'paid',
			'order.partial'     => 'partial',
			'order.overpaid'    => 'overpaid',
			'order.expired'     => 'expired',
		);

		if ( isset( $status_by_event_type[ $event_type ] ) ) {
			$this->apply_order_status_event( $order, $status_by_event_type[ $event_type ] );
			return;
		}

		if ( 'order.double_spend_detected' === $event_type ) {
			// No status change here - see this method's own doc comment for
			// why. Just the loud note a merchant needs to actually notice
			// the fraud case happened, per this step's own brief.
			$order->add_order_note(
				__(
					'Monokulo: FRAUD ALERT - a payment previously credited to this order was proven to be a double-spend and has been voided by the engine. If this changed the order\'s payment status, that change was announced separately. Please review this order.',
					'monokulo'
				)
			);
			return;
		}

		if ( 'order.double_spend_reversed' === $event_type ) {
			$txid = isset( $event['txid'] ) && is_string( $event['txid'] ) ? $event['txid'] : __( 'unknown', 'monokulo' );
			// Likewise no status change here - see this method's own doc
			// comment: any real status consequence of the reversal was
			// already announced via its own order.<status> event.
			$order->add_order_note(
				sprintf(
					/* translators: %s: the transaction id the earlier double-spend accusation was about */
					__(
						'Monokulo: an earlier double-spend accusation against this order (transaction %s) was a false positive and has been reversed by the engine. If this changed the order\'s payment status, that change was announced separately.',
						'monokulo'
					),
					$txid
				)
			);
			return;
		}

		// An event type this receiver doesn't recognize - logged, never
		// fatal. Forward-compatible with a future engine version adding a
		// new event type this plugin hasn't been updated for yet; the
		// delivery still gets a 200 (it was received and understood as far
		// as the envelope goes) rather than looping the engine's retry
		// worker forever over something a retry can never fix.
		$this->log( sprintf( 'Webhook: unrecognized event type "%s" - envelope accepted, no action taken.', $event_type ), 'warning' );
	}

	/**
	 * Applies one `order.<status>` transition event - the engine -> WooCommerce
	 * status mapping this whole step exists to build, and the reasoning
	 * behind every row of it.
	 *
	 * **`paid`/`overpaid` -> `WC_Order::payment_complete()`, not a plain
	 * `update_status()` call** (confirmed directly against the installed
	 * `includes/class-wc-order.php::payment_complete()`, not assumed from
	 * its name): this is WooCommerce's own canonical, real "a payment has
	 * been received" mechanism - every bundled and third-party gateway that
	 * actually receives real money calls this, not a raw status setter. It
	 * does meaningfully more than set a string: it sets `date_paid` (once,
	 * the first time it's ever called - `maybe_set_date_paid()`, read
	 * directly), reduces stock, records a transaction id, fires `woocommerce_
	 * payment_complete` for every other plugin that hooks it (subscriptions
	 * renewals, stock-alert plugins, accounting integrations), and -
	 * critically - **decides `processing` vs. `completed` itself**, via
	 * `$this->needs_processing()`: an order containing only virtual/
	 * downloadable items goes straight to `completed` (nothing left for the
	 * merchant to fulfill), while anything else goes to `processing`. This
	 * plugin has no principled way to know, order by order, whether that
	 * distinction applies - hardcoding `processing` unconditionally (which
	 * this step's own outcome text literally says, "ends with the WC order
	 * in processing/completed") would be *wrong* for exactly the
	 * downloadable-goods case that same outcome text's "/completed" already
	 * anticipates. `payment_complete()` is also idempotent by construction
	 * (gated on `$this->has_status( OrderStatus::PAYMENT_COMPLETE_STATUSES )`
	 * - `pending`/`on-hold`/`failed`/`cancelled` - read directly), so a
	 * duplicate call (which `event_already_applied()`'s dedupe should
	 * already prevent, but this is real defense in depth, not redundant
	 * caution) on an order already `processing`/`completed` is a safe no-op,
	 * not a second stock reduction or a second `date_paid` write.
	 *
	 * **`unconfirmed`/`confirming` -> `on-hold`**: both mean "the engine has
	 * seen the full amount, but doesn't yet trust it enough to treat the
	 * order as paid" (`src/status.rs::derive_status`, read directly) -
	 * `unconfirmed` is mempool-only, `confirming` is mined but below the
	 * tenant's required confirmation depth. WooCommerce's own `on-hold`
	 * status ("Awaiting payment confirmation" per its own `wc_get_order_
	 * statuses()` label) is a near-verbatim match for both, and - just as
	 * importantly - `on-hold` does **not** reduce stock the way `processing`/
	 * `completed` do (confirmed via `payment_complete()`'s own gate above,
	 * which only that method's success path ever reaches), which is exactly
	 * right: nothing should be reserved as sold for a payment this server
	 * doesn't yet trust. Collapsing both into the same WC bucket rather than
	 * inventing a distinct status for each is deliberate - a merchant acts
	 * on these identically ("wait"), and WooCommerce has no built-in status
	 * more specific than `on-hold` to distinguish them with anyway.
	 *
	 * **`partial` -> `on-hold`**, not `pending`: `pending` (a fresh order
	 * that's never received a single piconero) and `partial` (real,
	 * on-chain-or-mempool funds have arrived, just not enough of them) are
	 * genuinely different situations for a merchant - the former needs
	 * nothing from anyone yet, the latter may need a human to eventually
	 * decide what to do about an underpayment (there is no automated refund
	 * path anywhere in this system - `docs/DESIGN.md` §3, and `status.rs`'s
	 * own module doc comment cites the same fact). `on-hold` is
	 * WooCommerce's own general "needs a look" bucket for exactly this kind
	 * of ambiguity, and the order note added below says the actual reason,
	 * so a merchant scanning their on-hold queue isn't left guessing which
	 * of several possible causes put a given order there.
	 *
	 * **`expired` -> `cancelled`**: WooCommerce's own status for "this order
	 * did not complete and is not going to" - the direct match for an order
	 * whose payment window has closed with insufficient funds. Per `status.
	 * rs`'s own comment (read directly): "even a partial payment past the
	 * deadline surfaces as expired - the funds still exist at the address
	 * and require manual merchant handling, since no automated refund path
	 * exists" - restated in this transition's own order note below so that
	 * fact isn't buried in a status label alone.
	 *
	 * **`pending`**: maps to WooCommerce's own `pending` - normally a no-op
	 * (a fresh order is already `pending`), but reachable as a genuine
	 * *regression* if a double-spend void removes an order's only payment
	 * entirely (`derive_status`'s own `total == 0` branch). This is
	 * deliberately not special-cased away: the webhook is reporting the
	 * engine's own current, authoritative truth, and an order that no
	 * longer has any valid payment genuinely isn't paid anymore, however
	 * unusual that regression is in practice.
	 *
	 * Every branch's `$note` is handed to `WC_Order::update_status( $status,
	 * $note )`, not a separate `add_order_note()` call - confirmed directly
	 * (`includes/class-wc-order.php::add_status_transition_note()`) that
	 * WooCommerce folds this text into the *same* order note as its own
	 * auto-generated "Order status changed from X to Y." (`trim( $transition
	 * ['note'] . ' ' . $note )`), and skips adding any note at all when the
	 * status doesn't actually change (e.g. `unconfirmed` following
	 * `confirming`, both mapping to `on-hold`) - exactly right, since a
	 * same-bucket transition has nothing new to tell the merchant.
	 *
	 * @param WC_Order $order         The already-matched order.
	 * @param string   $engine_status One of `src/status.rs::OrderStatus`'s
	 *                                 seven `as_str()` values.
	 */
	private function apply_order_status_event( WC_Order $order, $engine_status ) {
		if ( 'paid' === $engine_status || 'overpaid' === $engine_status ) {
			if ( 'overpaid' === $engine_status ) {
				// payment_complete() has no note parameter of its own (it
				// writes a fixed "Payment via ..." note) - this is added as
				// a separate note specifically to flag the one fact that
				// method can't express: the customer sent more than was
				// due, and nothing in this plugin or the engine refunds the
				// excess automatically.
				$order->add_order_note(
					__(
						'Monokulo: the customer sent more Monero than the amount due. The excess is not automatically refunded - handle any refund manually.',
						'monokulo'
					)
				);
			}
			$order->payment_complete();
			return;
		}

		if ( ! isset( self::STATUS_MAP[ $engine_status ] ) ) {
			// Defensive only - every OrderStatus variant is covered by
			// either this branch or the paid/overpaid one above, so this
			// should be unreachable for a real engine payload. Logged, not
			// fatal, for the same forward-compatibility reason unrecognized
			// event types are in apply_webhook_event() above.
			$this->log( sprintf( 'Webhook: unrecognized engine status "%s" - no WooCommerce status change applied.', $engine_status ), 'warning' );
			return;
		}

		$note = '';
		if ( 'partial' === $engine_status ) {
			$note = __( 'Monokulo: a partial payment was received - the full amount due has not yet arrived.', 'monokulo' );
		} elseif ( 'expired' === $engine_status ) {
			$note = __(
				'Monokulo: this order\'s payment window has closed without full payment. If any funds were received, they remain at the payment address - there is no automatic refund; handle manually.',
				'monokulo'
			);
		}

		$order->update_status( self::STATUS_MAP[ $engine_status ], $note );
	}

	/**
	 * This gateway's own settings screen URL
	 * (`admin.php?page=wc-settings&tab=checkout&section={id}` -
	 * WooCommerce's standard, stable routing for a single gateway's settings
	 * panel), with `$extra_args` appended - used by `process_connect_return()`
	 * to build the redirect target that also carries the
	 * success/error flag `maybe_render_connect_notice()` reads.
	 *
	 * @param array $extra_args Query args to append, e.g.
	 *                           `array( 'monokulo_connected' => '1' )`.
	 * @return string
	 */
	private function build_settings_url( array $extra_args ) {
		return add_query_arg(
			$extra_args,
			admin_url( 'admin.php?page=wc-settings&tab=checkout&section=' . $this->id )
		);
	}

	/**
	 * Flashes a one-line success/error notice on this gateway's own settings
	 * screen right after a connect attempt redirects back to it - reads the
	 * plain query-string flags `process_connect_return()` appends
	 * (`build_settings_url()`) rather than using `WC_Admin_Settings::
	 * add_error()`, which only accumulates messages within the single
	 * request that renders the settings form and has no mechanism to carry
	 * a message across the separate `admin-post.php` request/redirect this
	 * callback is. This is the same "flag in the redirect target, read back
	 * on the next page load" pattern real WordPress admin screens
	 * (including WooCommerce's own core settings pages) use for exactly
	 * this situation.
	 *
	 * Scoped to this gateway's own settings screen specifically (checked via
	 * `$_GET['page']`/`$_GET['section']`) so this notice never appears on an
	 * unrelated admin screen just because these query flags happen to still
	 * be present in the URL (e.g. a bookmarked/shared link).
	 */
	public function maybe_render_connect_notice() {
		if ( ! isset( $_GET['page'], $_GET['section'] ) || 'wc-settings' !== $_GET['page'] || $this->id !== $_GET['section'] ) { // phpcs:ignore WordPress.Security.NonceVerification.Recommended
			return;
		}

		if ( isset( $_GET['monokulo_connected'] ) ) { // phpcs:ignore WordPress.Security.NonceVerification.Recommended
			echo '<div class="notice notice-success is-dismissible"><p>' .
				esc_html__( 'Your Monero wallet is connected - Monero payments are now enabled.', 'monokulo' ) .
				'</p></div>';
		} elseif ( isset( $_GET['monokulo_connect_error'] ) ) { // phpcs:ignore WordPress.Security.NonceVerification.Recommended
			echo '<div class="notice notice-error is-dismissible"><p>' .
				esc_html__( 'Could not connect your Monero wallet. Please try again.', 'monokulo' ) .
				'</p></div>';
		}
	}

	/**
	 * Thin wrapper around `wc_get_logger()` - guarded by `function_exists()`
	 * only for the same reason the rest of this class never fatals outside a
	 * real WooCommerce context, not because a live WooCommerce install (the
	 * only place `process_payment()` can actually run) might lack it.
	 *
	 * @param string $message Log message. Deliberately never includes raw
	 *                         customer/order PII beyond what's already an
	 *                         order note-worthy fact (order id, payment_id).
	 * @param string $level   A `WC_Log_Levels` level ('info'|'error'|...).
	 */
	private function log( $message, $level = 'info' ) {
		if ( function_exists( 'wc_get_logger' ) ) {
			wc_get_logger()->log( $level, $message, array( 'source' => 'monokulo' ) );
		}
	}
}
