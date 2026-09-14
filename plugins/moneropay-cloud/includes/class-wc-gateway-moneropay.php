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
 * Registers "Monero (via MoneroPay Cloud)" as a WooCommerce checkout option.
 *
 * WBS 1.5.1 scope only: this class exists to *register* with WooCommerce and
 * expose the settings fields a merchant will fill in once the connect flow
 * (WBS 1.5.3) exists. It does not yet talk to the engine at all -
 * `process_payment()` (the method WooCommerce calls when a customer actually
 * checks out with this gateway selected) is WBS 1.5.2's job, and until that
 * lands this class deliberately leaves WooCommerce's own default
 * `process_payment()` behavior in place (inherited, unoverridden, from
 * `WC_Payment_Gateway` - which does not attempt to process anything on its
 * own). Shipping this gateway *disabled* by default is not a placeholder
 * left half-finished - it is WBS 1.5.1's own stated acceptance outcome
 * ("disabled state is fine at this step"), matching the real end-to-end
 * plan in `docs/WOOCOMMERCE_ROADMAP.md` Stage 8: the gateway is only meant
 * to become enabled once the merchant has actually connected a wallet
 * through the control plane, not the moment this plugin is installed.
 *
 * Every real decision - order creation, payment matching, exchange rates,
 * webhook delivery - lives in the Rust engine (`moneropay-core`) and the
 * control plane sitting in front of it, per this repo's Stage 7 framing
 * ("PHP is a thin adapter, not a second implementation of anything"). This
 * class's job stops at being a faithful WooCommerce citizen: register
 * correctly, expose the right settings fields, and (starting at 1.5.2) make
 * one outbound HTTP call per order. Nothing more belongs here by design.
 */
class WC_Gateway_MoneroPay extends WC_Payment_Gateway {

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
		);
	}
}
