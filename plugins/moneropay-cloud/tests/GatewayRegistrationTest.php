<?php
/**
 * WBS 1.5.1's acceptance test: the gateway actually registers with
 * WooCommerce.
 *
 * **Naming this file `GatewayRegistrationTest.php` (class-name-first, no
 * `test-` prefix) rather than the WordPress-core-style `test-gateway-
 * registration.php` (with `Test_Gateway_Registration` underscored inside)
 * this file originally used is a fix for a real failure hit while getting
 * this suite to actually run, not a style preference.** PHPUnit 10's own
 * `Runner\TestSuiteLoader::load()` (`vendor/phpunit/phpunit/src/Runner/
 * TestSuiteLoader.php`, read directly, not assumed) derives an expected
 * class-name suffix from each discovered file's own basename
 * (`classNameFromFileName()` - strip `.php`, truncate at the first literal
 * `.`) and then requires the file's declared class's short name to
 * case-insensitively **end with** that literal string, or it throws
 * `ClassCannotBeFoundException` - which is exactly the "Class
 * test-gateway-registration cannot be found" failure this suite produced
 * for real (`wp-env run tests-cli --env-cwd=wp-content/plugins/
 * moneropay-cloud phpunit`) with the original naming, even though the class
 * loaded correctly and `class_exists()` was confirmed `true` when the same
 * bootstrap + file were `require`d directly by hand - the class was never
 * missing, PHPUnit's own literal-suffix check just never matched a hyphen in
 * a filename against an underscore in a class name. Filename-equals-class-
 * name (this file's actual current naming) sidesteps the whole check by
 * construction rather than working around a specific hyphen/underscore
 * mismatch. `phpunit.xml.dist`'s own `<directory suffix="Test.php">` pattern
 * was updated to match this convention.
 *
 * @package MoneroPayCloud
 */

/**
 * Asserts `WC_Gateway_MoneroPay` is a real, registered WooCommerce payment
 * gateway once this plugin is active.
 *
 * **The `payment_gateways()` vs. `get_available_payment_gateways()` question,
 * resolved against WooCommerce's own real source, not guessed** - read
 * directly from the installed copy at
 * `wp-content/plugins/woocommerce/includes/class-wc-payment-gateways.php`
 * (the exact file this test's own WordPress install downloads, per
 * `.wp-env.json`'s `"plugins"` entry pointing at
 * `woocommerce.latest-stable.zip` - not a vendored guess, the real package
 * `wp-env` pulls down for this environment):
 *
 * - `WC_Payment_Gateways::payment_gateways()` returns `$this->payment_gateways`
 *   directly - the full set of instantiated gateway objects built from every
 *   class name the `woocommerce_payment_gateways` filter returned, in
 *   `init()`, with **no** enabled/applicable filtering applied at all.
 * - `WC_Payment_Gateways::get_available_payment_gateways()` calls
 *   `payment_gateways()` and then filters that list down with
 *   `is_available()` (which, for the base `WC_Payment_Gateway` class,
 *   returns `$this->enabled === 'yes'` as its first and controlling check)
 *   plus a currency-restriction check - i.e. it deliberately excludes any
 *   disabled gateway.
 *
 * WBS 1.5.1's own stated outcome is explicit that a *disabled* gateway is an
 * acceptable end state for this step ("Monero (via MoneroPay Cloud)" showing
 * up, "disabled state is fine at this step") - and `WC_Gateway_MoneroPay`
 * genuinely ships disabled by default (see the constructor's own comment on
 * `$this->enabled` in `includes/class-wc-gateway-moneropay.php`), since there
 * is no connected wallet yet for it to process a real payment against. Given
 * that, asserting this plugin's gateway ID appears in
 * `get_available_payment_gateways()`'s result would be asserting something
 * this step's own disabled-by-default choice makes false by construction -
 * not a bug in the gateway, but a mismatch between *that specific* WooCommerce
 * method's real, source-confirmed behavior and what this step is actually
 * meant to prove. `payment_gateways()` - "every gateway WooCommerce knows
 * about, registered, regardless of enabled state" - is the one that actually
 * matches the WBS's own outcome language ("shows... as a checkout option
 * (disabled state is fine)"): being a real, registered checkout option is
 * exactly what `payment_gateways()` reports, independent of `enabled`.
 * That's why this test asserts against `payment_gateways()`, not
 * `get_available_payment_gateways()` - a deliberate reading of the WBS's own
 * phrasing against WooCommerce's real, checked method semantics, not the
 * WBS's literal string "available-gateways list" taken as a specific method
 * name it never actually names.
 *
 * (A second, later-step consideration worth flagging for whoever picks up
 * WBS 1.5.3: once the gateway is enabled for real after a connect flow,
 * `get_available_payment_gateways()` becomes the right assertion for proving
 * it's actually offered to a customer at checkout - that's a different,
 * future test, not a gap in this one.)
 */
class GatewayRegistrationTest extends WP_UnitTestCase {

	/**
	 * The gateway's real, checked-not-hardcoded-twice identifier - re-derives
	 * it from the class itself (by instantiating one and reading `->id`)
	 * rather than repeating the literal string `'moneropay_cloud'` a second
	 * time in this file, so a future rename of the constant in
	 * `class-wc-gateway-moneropay.php` can't silently desync from what this
	 * test asserts against.
	 */
	private function get_expected_gateway_id() {
		$gateway = new WC_Gateway_MoneroPay();
		return $gateway->id;
	}

	/**
	 * The core WBS 1.5.1 acceptance assertion: once this plugin is active,
	 * WooCommerce's own gateway registry (`WC_Payment_Gateways::instance()`)
	 * knows about `WC_Gateway_MoneroPay` - proving the
	 * `woocommerce_payment_gateways` filter hook in `moneropay-cloud.php`
	 * genuinely reaches WooCommerce's real registration machinery, not just
	 * that the PHP class itself is syntactically valid and instantiable in
	 * isolation (a test that only did `new WC_Gateway_MoneroPay()` would pass
	 * even if the plugin bootstrap file never hooked the filter at all - this
	 * one specifically would not).
	 */
	public function test_gateway_is_registered_with_woocommerce() {
		$expected_id = $this->get_expected_gateway_id();

		// `WC_Payment_Gateways::instance()` is WooCommerce's own singleton
		// accessor for its gateway registry - the same object `WC()->payment_
		// gateways` on the global `WooCommerce` instance points at. Going
		// through this accessor (rather than constructing a fresh
		// `WC_Payment_Gateways` by hand) is what actually exercises the real,
		// already-initialized registry WooCommerce's own checkout code reads
		// from - `->init()` already ran when this object was first
		// constructed during WordPress's normal bootstrap, applying the
		// `woocommerce_payment_gateways` filter (and therefore this plugin's
		// `moneropay_cloud_add_gateway_class()` callback) at that point.
		$registered_gateways = WC_Payment_Gateways::instance()->payment_gateways();

		$registered_ids = wp_list_pluck( $registered_gateways, 'id' );

		$this->assertContains(
			$expected_id,
			$registered_ids,
			'Expected WC_Gateway_MoneroPay (id "' . $expected_id . '") to appear in ' .
			'WC_Payment_Gateways::payment_gateways() - the full registered-gateway list ' .
			'WooCommerce builds from the woocommerce_payment_gateways filter. Its absence ' .
			'means either the plugin\'s add_filter() call in moneropay-cloud.php never ran, ' .
			'or WooCommerce itself failed to load in this test environment.'
		);

		// Cross-checked against the actual registered object, not just its id
		// string, so a hypothetical id collision with an unrelated gateway
		// couldn't silently pass this test.
		$this->assertInstanceOf(
			'WC_Gateway_MoneroPay',
			$registered_gateways[ $expected_id ],
			'The registered gateway at id "' . $expected_id . '" should be a real ' .
			'WC_Gateway_MoneroPay instance, not merely an id string collision.'
		);
	}

	/**
	 * Confirms this step's own stated outcome ("disabled state is fine at
	 * this step") describes what actually ships, not just what's tolerated -
	 * asserting the negative here (rather than only the positive
	 * registration test above) is what would catch an accidental
	 * `'default' => 'yes'` regression in `init_form_fields()` before it ever
	 * reached a real deployment, where an enabled-with-no-connected-wallet
	 * gateway would let a customer select a payment method WBS 1.5.2's
	 * `process_payment()` cannot yet honor.
	 */
	public function test_gateway_is_disabled_by_default() {
		$gateway = new WC_Gateway_MoneroPay();

		$this->assertSame(
			'no',
			$gateway->enabled,
			'WC_Gateway_MoneroPay should ship disabled by default (WBS 1.5.1\'s own stated ' .
			'outcome) - there is no connected wallet for a fresh install to process a real ' .
			'payment against yet.'
		);

		// The behavioral consequence of the above, checked directly against
		// WooCommerce's own real is_available() rather than re-deriving it:
		// a disabled gateway is correctly excluded from
		// get_available_payment_gateways() (the *other* method this test
		// file's own class doc comment discusses at length) - included here
		// as a companion, not a replacement, for the main registration test
		// above, so both of WooCommerce's real, distinct gateway-listing
		// methods are exercised by this suite, each checked for the behavior
		// that's actually true of it.
		$this->assertArrayNotHasKey(
			$gateway->id,
			WC_Payment_Gateways::instance()->get_available_payment_gateways(),
			'A disabled gateway should not appear in get_available_payment_gateways() - ' .
			'confirms this method really does filter on enabled state, so the reasoning in ' .
			'this file\'s class doc comment for testing against payment_gateways() instead ' .
			'is checked behavior, not just an untested claim about WooCommerce\'s source.'
		);
	}
}
