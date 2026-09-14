<?php
/**
 * `WC_Gateway_MoneroPay`: the WooCommerce-facing shell for MoneroPay Cloud.
 *
 * @package MoneroPayCloud
 */

if ( ! defined( 'ABSPATH' ) ) {
	exit;
}

/**
 * Registers "Monero (via MoneroPay Cloud)" as a WooCommerce checkout option
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
 * webhook delivery - lives in the Rust engine (`moneropay-core`) and the
 * control plane sitting in front of it, per this repo's Stage 7 framing
 * ("PHP is a thin adapter, not a second implementation of anything"). This
 * class's job stops at being a faithful WooCommerce citizen: register
 * correctly, expose the right settings fields, and make one outbound HTTP
 * call per order. Nothing more belongs here by design - in particular, this
 * gateway never decides an order is paid (see `process_payment()`'s own doc
 * comment on why `payment_complete()` is never called here) and never
 * verifies a payment itself (WBS 1.5.4's webhook receiver's job).
 */
class WC_Gateway_MoneroPay extends WC_Payment_Gateway {

	/**
	 * The one, fixed address of MoneroPay Cloud's own hosted control plane -
	 * the service `control-plane/src/http/connect.rs` implements, sitting in
	 * front of (potentially many) real engines. **Not** the same thing as
	 * `$api_base_url`/`endpoint` below, and the two must never be conflated:
	 * `$api_base_url` is *an engine's* base URL (there can be many - every
	 * tenant's own hosted engine, or a self-hoster's own instance); this
	 * constant is *the* control plane's base URL, and for the hosted
	 * "MoneroPay Cloud" product this plugin exists for
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
	 * this step, MoneroPay Cloud's control plane has no real, decided
	 * production domain yet (checked `docs/WOOCOMMERCE_ROADMAP.md` and
	 * `work_notes.md` directly - neither names one). `cloud.moneropay.example`
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
	const CONTROL_PLANE_BASE_URL = 'https://cloud.moneropay.example';

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
	const CONNECT_RETURN_ACTION = 'moneropay_cloud_connect_return';

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
	 * Sets up the gateway's identity and settings fields.
	 *
	 * WooCommerce instantiates every class registered via the
	 * `woocommerce_payment_gateways` filter (see `moneropay-cloud.php`)
	 * with no constructor arguments, so every property this gateway needs
	 * has to be derivable here with no external input beyond WordPress's
	 * own options table (via `get_option()`, populated by
	 * `init_settings()` below) - there is no other injection point at
	 * this stage of WooCommerce's own lifecycle.
	 */
	public function __construct() {
		// `id` is the gateway's permanent identifier: it's the array key
		// WooCommerce's settings are stored under
		// (`woocommerce_moneropay_cloud_settings` in `wp_options`), the
		// value that ends up as `$order->get_payment_method()` on every
		// order this gateway processes, and the suffix of the webhook
		// endpoint WBS 1.5.4 registers (`woocommerce_api_{$this->id}`).
		// Changing it later would silently orphan every existing order's
		// payment-method record and any already-registered webhook URL, so
		// it's treated as fixed from this first step onward, not a cosmetic
		// label. Chosen to read as "the id", not a version-agnostic English
		// phrase like "monero" (which a future unrelated Monero gateway
		// could just as reasonably claim) - `moneropay_cloud` names the
		// actual product per `docs/WOOCOMMERCE_ROADMAP.md`'s own framing of
		// the hosted service by that name throughout.
		$this->id = 'moneropay_cloud';

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
		$this->method_title       = __( 'MoneroPay Cloud', 'moneropay-cloud' );
		$this->method_description = __(
			'Accept Monero at checkout without ever holding customer funds or your own spend key - MoneroPay Cloud watches the chain for you and only ever needs a view key. Connect your wallet to enable this gateway for real.',
			'moneropay-cloud'
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
		// ("Monero (via MoneroPay Cloud)"), so a fresh install already shows
		// the right label with zero configuration, while still letting a
		// merchant override the wording later without a code change.
		$this->title       = $this->get_option( 'title', __( 'Monero (via MoneroPay Cloud)', 'moneropay-cloud' ) );
		$this->description = $this->get_option(
			'description',
			__( 'Pay with Monero. You will be redirected to a secure MoneroPay Cloud payment page to complete your purchase.', 'moneropay-cloud' )
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
	 * settings screen (Settings > Payments > MoneroPay Cloud) and that
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
				'title'   => __( 'Enable/Disable', 'moneropay-cloud' ),
				'type'    => 'checkbox',
				'label'   => __( 'Enable Monero payments via MoneroPay Cloud', 'moneropay-cloud' ),
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
				'title'       => __( 'Title', 'moneropay-cloud' ),
				'type'        => 'text',
				'description' => __( 'The label the customer sees for this payment method at checkout.', 'moneropay-cloud' ),
				'default'     => __( 'Monero (via MoneroPay Cloud)', 'moneropay-cloud' ),
				'desc_tip'    => true,
			),
			'description' => array(
				'title'       => __( 'Description', 'moneropay-cloud' ),
				'type'        => 'textarea',
				'description' => __( 'The explanatory text the customer sees for this payment method at checkout.', 'moneropay-cloud' ),
				'default'     => __( 'Pay with Monero. You will be redirected to a secure MoneroPay Cloud payment page to complete your purchase.', 'moneropay-cloud' ),
				'desc_tip'    => true,
			),

			'connection'  => array(
				'title'       => __( 'Connection', 'moneropay-cloud' ),
				'type'        => 'title',
				'description' => __( 'Click below to connect your Monero wallet through MoneroPay Cloud - this fills in everything below automatically and enables this gateway. Advanced/self-hosted users can also enter these by hand instead.', 'moneropay-cloud' ),
			),

			// The real WBS 1.5.3 button - see `generate_moneropay_connect_html()`'s
			// own doc comment for exactly what it renders and why this needs
			// a genuinely custom field type rather than reusing `'title'`
			// (whose own `generate_title_html()` renders fixed markup this
			// step needs to deviate from: a real `<a href=...>` built fresh
			// on every render, not static text).
			'connect'     => array(
				'type'        => 'moneropay_connect',
				'description' => __( 'You will be sent to MoneroPay Cloud to sign in (or sign up) and confirm the connection, then returned here automatically.', 'moneropay-cloud' ),
			),

			'endpoint'    => array(
				'title'       => __( 'Engine API base URL', 'moneropay-cloud' ),
				'type'        => 'text',
				'description' => __( 'The base URL of the MoneroPay Cloud engine this store talks to (no trailing slash needed). Filled in automatically by Connect above - only edit this by hand for a self-hosted engine.', 'moneropay-cloud' ),
				'default'     => '',
				'placeholder' => 'https://pay.example.com',
				'desc_tip'    => true,
			),
			'public_key'  => array(
				'title'       => __( 'Tenant public key', 'moneropay-cloud' ),
				'type'        => 'text',
				'description' => __( 'Your MoneroPay Cloud tenant\'s public key (starts with pk_). Filled in automatically by Connect above.', 'moneropay-cloud' ),
				'default'     => '',
				'placeholder' => 'pk_...',
				'desc_tip'    => true,
			),
			'secret_token' => array(
				'title'       => __( 'Tenant secret key', 'moneropay-cloud' ),
				'type'        => 'password',
				'description' => __( 'Your MoneroPay Cloud tenant\'s secret key (starts with sk_). Filled in automatically by Connect above. Not currently used by this plugin for anything - kept so a future update never has to ask you to reconnect just to retrieve it again.', 'moneropay-cloud' ),
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
	 * - factored out of `is_available()` so `generate_moneropay_connect_html()`
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
			throw new Exception( __( 'Could not load this order to start the Monero payment.', 'moneropay-cloud' ) );
		}

		$engine_order = $this->create_engine_order( $order );

		// The one point in this gateway's whole flow that ever sees the
		// mapping between a WC order and the engine's own payment_id -
		// recorded now, while it's in hand, so it isn't thrown away.
		// WBS 1.5.4's webhook receiver will need exactly this lookup (an
		// incoming delivery carries a payment_id, not a WC order id) to know
		// which order to update; nothing here *consumes* that meta key yet -
		// that consumption is 1.5.4's job, not built here.
		$order->update_meta_data( '_moneropay_cloud_payment_id', $engine_order['payment_id'] );
		$order->add_order_note(
			sprintf(
				/* translators: %s: MoneroPay Cloud payment_id */
				__( 'Customer redirected to MoneroPay Cloud checkout for payment_id %s.', 'moneropay-cloud' ),
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
				__( 'Monero payments are not fully configured for this store yet. Please contact the store owner.', 'moneropay-cloud' )
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
				__( 'Order #%1$s on %2$s', 'moneropay-cloud' ),
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
				__( 'Could not reach MoneroPay Cloud to start this payment. Please try again shortly.', 'moneropay-cloud' )
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
				__( 'MoneroPay Cloud could not start this payment. Please contact the store or try again.', 'moneropay-cloud' )
			);
		}

		$decoded = json_decode( $raw_body, true );
		if ( ! is_array( $decoded ) || empty( $decoded['payment_id'] ) ) {
			$this->log(
				sprintf( 'Order creation response from %s was not the expected shape: %s', $request_url, $raw_body ),
				'error'
			);
			throw new Exception(
				__( 'MoneroPay Cloud returned an unexpected response. Please contact the store.', 'moneropay-cloud' )
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
	 * `moneropay_cloud_control_plane_base_url` - a filter, not just the bare
	 * constant, for two concrete reasons: it's what lets this plugin's own
	 * tests point `process_connect_return()`/`generate_moneropay_connect_html()`
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
		return apply_filters( 'moneropay_cloud_control_plane_base_url', self::CONTROL_PLANE_BASE_URL );
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
	 *    (`generate_moneropay_connect_html()`/`admin_options()`), which is
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
	 * stored under between `generate_moneropay_connect_html()` (which mints
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
		return 'moneropay_cloud_connect_nonce_' . $this->id;
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
	 * genuinely custom `WC_Settings_API` field type (`'moneropay_connect'`,
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
	public function generate_moneropay_connect_html( $key, $data ) {
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
				<label for="<?php echo esc_attr( $field_key ); ?>"><?php esc_html_e( 'Connect your Monero wallet', 'moneropay-cloud' ); ?></label>
			</th>
			<td class="forminp">
				<?php if ( $this->has_credentials() ) : ?>
					<p>
						<?php esc_html_e( 'Connected as', 'moneropay-cloud' ); ?>
						<code><?php echo esc_html( $this->tenant_public_key ); ?></code>
					</p>
				<?php endif; ?>
				<a href="<?php echo esc_url( $connect_url ); ?>" id="<?php echo esc_attr( $field_key ); ?>" class="button button-primary">
					<?php
					echo $this->has_credentials()
						? esc_html__( 'Reconnect your Monero wallet', 'moneropay-cloud' )
						: esc_html__( 'Connect your Monero wallet', 'moneropay-cloud' );
					?>
				</a>
				<?php echo $this->get_description_html( $data ); // WPCS: XSS ok. ?>
			</td>
		</tr>
		<?php
		return ob_get_clean();
	}

	/**
	 * `'connect'`'s own `type` (`moneropay_connect`) has no real form input
	 * for `process_admin_options()` to read back on save - it only ever
	 * renders a link. Without this method, `get_field_value()` would fall
	 * through to `validate_text_field()` on a `null` POST value (checked
	 * directly, `abstract-wc-settings-api.php::get_field_value()`), which is
	 * harmless but pointless (nothing should ever read a
	 * `moneropay_connect` settings key) and, on PHP 8.1+, a `null`-to-string
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
	public function validate_moneropay_connect_field( $key, $value ) {
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
			wp_die( esc_html__( 'You do not have permission to do this.', 'moneropay-cloud' ), '', array( 'response' => 403 ) );
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
	 *                 `moneropay_cloud_connected` or
	 *                 `moneropay_cloud_connect_error` flag appended for
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
			return $this->build_settings_url( array( 'moneropay_cloud_connect_error' => 'nonce' ) );
		}

		$finish = $this->call_connect_finish( $token );
		if ( null === $finish ) {
			return $this->build_settings_url( array( 'moneropay_cloud_connect_error' => 'finish' ) );
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

		return $this->build_settings_url( array( 'moneropay_cloud_connected' => '1' ) );
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
	 * This gateway's own settings screen URL
	 * (`admin.php?page=wc-settings&tab=checkout&section={id}` -
	 * WooCommerce's standard, stable routing for a single gateway's settings
	 * panel), with `$extra_args` appended - used by `process_connect_return()`
	 * to build the redirect target that also carries the
	 * success/error flag `maybe_render_connect_notice()` reads.
	 *
	 * @param array $extra_args Query args to append, e.g.
	 *                           `array( 'moneropay_cloud_connected' => '1' )`.
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

		if ( isset( $_GET['moneropay_cloud_connected'] ) ) { // phpcs:ignore WordPress.Security.NonceVerification.Recommended
			echo '<div class="notice notice-success is-dismissible"><p>' .
				esc_html__( 'Your Monero wallet is connected - Monero payments are now enabled.', 'moneropay-cloud' ) .
				'</p></div>';
		} elseif ( isset( $_GET['moneropay_cloud_connect_error'] ) ) { // phpcs:ignore WordPress.Security.NonceVerification.Recommended
			echo '<div class="notice notice-error is-dismissible"><p>' .
				esc_html__( 'Could not connect your Monero wallet. Please try again.', 'moneropay-cloud' ) .
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
			wc_get_logger()->log( $level, $message, array( 'source' => 'moneropay-cloud' ) );
		}
	}
}
