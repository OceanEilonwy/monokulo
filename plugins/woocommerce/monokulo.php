<?php
/**
 * Plugin Name:       Monokulo for WooCommerce
 * Plugin URI:        https://github.com/monokulo/scanner
 * Description:       Accept Monero at checkout via Monokulo, the hosted,
 *                     non-custodial payment gateway built on scanner.
 *                     No spend key ever touches this server or ours - only a
 *                     view key, used to watch for incoming payments.
 * Version:            0.1.0
 * Requires at least:  6.4
 * Requires PHP:       8.1
 * Requires Plugins:   woocommerce
 * WC requires at least: 8.0
 * Author:             Monokulo
 * Author URI:         https://github.com/monokulo
 * License:            GPL-2.0-or-later
 * License URI:        https://www.gnu.org/licenses/gpl-2.0.html
 * Text Domain:         monokulo
 * Domain Path:         /languages
 *
 * @package Monokulo
 */

// Exit if accessed directly - this file is a WordPress plugin entry point,
// never meant to be loaded outside a WordPress bootstrap. Every other PHP
// file in this plugin repeats this same guard for the same reason: WordPress
// plugin directories are web-servable by default on many hosts, and without
// this guard a request straight to this file's URL would execute arbitrary
// plugin bootstrap code (harmless here, but the convention exists precisely
// because it *isn't* always harmless across the plugin ecosystem).
if ( ! defined( 'ABSPATH' ) ) {
	exit;
}

/**
 * WBS 1.5.1 scope, deliberately: registration only. This bootstrap file's
 * entire job is making "Monero (via Monokulo)" appear in WooCommerce's
 * list of registered payment gateways - nothing here talks to the engine yet
 * (that's 1.5.2's `process_payment()`), and nothing here makes the gateway
 * enabled by default (also correct per the WBS's own stated outcome: "disabled
 * state is fine at this step"). Every actual payment decision still lives in
 * the Rust engine (`scanner`) per `docs/WOOCOMMERCE_ROADMAP.md` Stage 7's
 * "PHP is a thin adapter, not a second implementation of anything" framing -
 * this file, and the gateway class it loads, are deliberately the thinnest
 * possible WordPress-side shim over that engine, not a growing PHP business
 * layer.
 */

/**
 * Loads the gateway class and registers it with WooCommerce.
 *
 * Deferred to the `plugins_loaded` hook rather than run at the top level of
 * this file for a concrete reason, not just convention: `WC_Gateway_Monokulo`
 * (in includes/class-wc-gateway-monokulo.php) `extends WC_Payment_Gateway`,
 * a class WooCommerce itself defines. WordPress loads plugins in an
 * unspecified order relative to each other, so if this plugin happens to load
 * before WooCommerce does, `class WC_Payment_Gateway` wouldn't exist yet and
 * the `extends` would be a fatal error. `plugins_loaded` is the first hook
 * every active plugin is guaranteed to have already been `require`d by, so by
 * the time this callback runs, WooCommerce's own classes are available
 * regardless of which plugin's row in `wp_options` happened to load first.
 * The `Requires Plugins: woocommerce` header above (a WordPress 6.5+
 * mechanism) additionally stops this plugin from activating at all if
 * WooCommerce isn't installed, but that only prevents *activation* - it
 * doesn't reorder *load* order between two already-active plugins, so this
 * hook is still the right guard even with that header present.
 */
function monokulo_init_gateway_class() {
	if ( ! class_exists( 'WC_Payment_Gateway' ) ) {
		// WooCommerce isn't active. Fail silently rather than fatal - the
		// `Requires Plugins` header (WP 6.5+) already stops a fresh activation
		// in this state, but an older WordPress core, or WooCommerce being
		// deactivated *after* this plugin was already active, both reach this
		// function without that guard having fired. Nothing downstream of this
		// early return runs, so there's nothing left to register.
		return;
	}

	require_once __DIR__ . '/includes/class-wc-gateway-monokulo.php';
}
add_action( 'plugins_loaded', 'monokulo_init_gateway_class' );

/**
 * Adds `WC_Gateway_Monokulo` to WooCommerce's list of available gateway
 * classes via the standard `woocommerce_payment_gateways` filter - this is
 * the actual registration mechanism WooCommerce documents for third-party
 * gateways (WooCommerce itself iterates this filter's return value and
 * instantiates every class name / object in it), confirmed against
 * WooCommerce's own `WC_Payment_Gateways::init()` source
 * (`class-wc-payment-gateways.php`) rather than assumed from memory - see
 * this plugin's `tests/GatewayRegistrationTest.php` for the reasoning
 * behind exactly which of WooCommerce's own two gateway-listing methods this
 * plugin's test asserts against, and why.
 *
 * @param string[] $gateways Fully-qualified class names (or gateway instances)
 *                           WooCommerce will offer at checkout.
 * @return string[] The same list, with this plugin's gateway appended.
 */
function monokulo_add_gateway_class( $gateways ) {
	$gateways[] = 'WC_Gateway_Monokulo';
	return $gateways;
}
add_filter( 'woocommerce_payment_gateways', 'monokulo_add_gateway_class' );

/**
 * Tells WooCommerce managers, on every admin screen, when this store was
 * connected by an older plugin version and must reconnect before it can
 * take Monero payments again (see `WC_Gateway_Monokulo::CONNECTION_VERSION`).
 * Hooked here rather than in the gateway's constructor so it shows whether
 * or not WooCommerce builds its gateways on the current request.
 */
function monokulo_render_reconnect_notice() {
	if ( class_exists( 'WC_Gateway_Monokulo' ) ) {
		WC_Gateway_Monokulo::render_reconnect_notice();
	}
}
add_action( 'admin_notices', 'monokulo_render_reconnect_notice' );
