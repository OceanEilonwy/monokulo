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
	}

	/**
	 * Defines the settings fields WooCommerce renders on this gateway's own
	 * settings screen (Settings > Payments > MoneroPay Cloud) and that
	 * `init_settings()`/`get_option()` above read defaults from.
	 *
	 * Kept to the minimum this step actually needs, not a placeholder set of
	 * every field a *later* step will eventually want (connect-flow
	 * credentials, an endpoint override, a webhook secret) - those belong to
	 * WBS 1.5.3, once there is real connect-flow logic to back them.
	 * Building empty settings fields now, with nothing yet reading or
	 * writing them for real, would be exactly the kind of unrequested scope
	 * this project's own established practice (see `work_notes.md`'s
	 * progress log) has repeatedly flagged and avoided elsewhere.
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
				// documentation of an intention.
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

			// WBS 1.5.2 adds these two fields, and only these two - see
			// `$api_base_url`'s doc comment above for the full reasoning.
			// This is a real gap 1.5.2 has to resolve to have anything to
			// call at all, not scope creep toward a finished credentials UX:
			// no webhook-secret field, no admin secret-key field, nothing
			// else a later step (1.5.3's real connect flow, 1.5.4's webhook
			// receiver) will eventually need - each of those gets its own
			// field only once the step that actually uses it exists.
			'connection'  => array(
				'title'       => __( 'Connection (temporary manual setup)', 'moneropay-cloud' ),
				'type'        => 'title',
				'description' => __( 'A future update will replace these two fields with a one-click "Connect your Monero wallet" button. Until then, enter them by hand from your MoneroPay Cloud dashboard.', 'moneropay-cloud' ),
			),
			'endpoint'    => array(
				'title'       => __( 'Engine API base URL', 'moneropay-cloud' ),
				'type'        => 'text',
				'description' => __( 'The base URL of the MoneroPay Cloud engine this store talks to (no trailing slash needed).', 'moneropay-cloud' ),
				'default'     => '',
				'placeholder' => 'https://pay.example.com',
				'desc_tip'    => true,
			),
			'public_key'  => array(
				'title'       => __( 'Tenant public key', 'moneropay-cloud' ),
				'type'        => 'text',
				'description' => __( 'Your MoneroPay Cloud tenant\'s public key (starts with pk_). Only the public key is ever entered here - order creation is a public endpoint and never needs your secret key.', 'moneropay-cloud' ),
				'default'     => '',
				'placeholder' => 'pk_...',
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
