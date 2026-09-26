<?php
/**
 * `process_payment()` creates the order on Monokulo
 * (`POST {endpoint}/pay/{pk}/orders`, authenticated with the store's secret
 * key) with the request WooCommerce's own real checkout flow would have
 * produced, and hands WooCommerce back Monokulo's checkout page,
 * `{endpoint}/pay/{pk}/orders/{order_id}`.
 *
 * **Why `pre_http_request`, not a hand-rolled HTTP client mock**: WordPress's
 * HTTP API (`wp_remote_post`/`wp_remote_get`, both of which ultimately go
 * through `WP_Http::request()`) is procedural, not object-oriented - there is
 * no injectable client object for a test to substitute. `WP_Http::request()`
 * itself (`wp-includes/class-http.php`, read directly) applies the
 * `pre_http_request` filter first and, if any callback returns anything other
 * than `false`, returns that value immediately without ever touching the
 * network - this is WordPress core's own documented short-circuit mechanism
 * for exactly this situation, not a workaround. A callback registered on this
 * filter receives the same `$args` `wp_remote_post()` was called with
 * (headers, body, method, timeout, ...) and the target `$url`, so the request
 * this gateway's `create_engine_order()` actually built can be asserted on
 * directly, and the canned array returned back stands in for a genuine engine
 * response, in the same shape `wp_remote_retrieve_body()`/
 * `wp_remote_retrieve_response_code()` already know how to read
 * (`array( 'body' => ..., 'response' => array( 'code' => ..., 'message' => ... ) )`).
 *
 * @package Monokulo
 */

/**
 * Asserts `WC_Gateway_Monokulo::process_payment()` against a real `WC_Order`
 * and a mocked engine response.
 */
class ProcessPaymentTest extends WP_UnitTestCase {

	/**
	 * The captured `wp_remote_post()` call this test's `pre_http_request`
	 * filter observed - `null` until a test's HTTP call actually happens, so
	 * an assertion on it failing loudly (rather than silently comparing
	 * against `null`) is itself proof the mocked filter really intercepted
	 * something.
	 *
	 * @var array{url: string, args: array}|null
	 */
	private $captured_request;

	public function set_up(): void {
		parent::set_up();
		$this->captured_request = null;
	}

	public function tear_down(): void {
		// Never let one test's mocked response leak into the next - every
		// test in this file registers its own `pre_http_request` callback,
		// so nothing should still be attached once a test finishes.
		remove_all_filters( 'pre_http_request' );
		parent::tear_down();
	}

	/**
	 * Builds a real, saved `WC_Order` (via WooCommerce's own `wc_create_order()`,
	 * not a hand-built stub) with a $42.50 USD total - the exact object
	 * `process_payment( $order->get_id() )` receives in production, since
	 * WooCommerce always calls it with an order id it then re-loads via
	 * `wc_get_order()` (see `WC_Checkout::process_order_payment()`).
	 *
	 * @return WC_Order
	 */
	private function create_real_order() {
		$order = wc_create_order();
		$this->assertNotWPError( $order, 'wc_create_order() should succeed inside a WP_UnitTestCase transaction.' );

		$order->set_currency( 'USD' );
		$order->set_total( '42.50' );
		$order->save();

		return $order;
	}

	/**
	 * Builds a `WC_Gateway_Monokulo` connected the way the current connect
	 * flow leaves it (`endpoint`, `public_key`, `secret_token` and
	 * `connection_version`) - `update_option()` persists to
	 * `wp_options` immediately (`WC_Settings_API::update_option()`, checked
	 * directly), so a *second*, freshly-constructed gateway object picks the
	 * values back up through its own constructor's normal
	 * `init_settings()`/`get_option()` path, exactly like a real merchant's
	 * saved settings would on the next request.
	 *
	 * @param string $endpoint   Monokulo's public address to configure.
	 * @param string $public_key The store public key to configure.
	 * @param string $version    The stored `connection_version` ('' for an
	 *                           install connected by an older plugin).
	 * @return WC_Gateway_Monokulo
	 */
	private function create_configured_gateway( $endpoint = 'http://monokulo.test', $public_key = 'pk_test_abc123', $version = WC_Gateway_Monokulo::CONNECTION_VERSION ) {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'enabled', 'yes' );
		$seed->update_option( 'endpoint', $endpoint );
		$seed->update_option( 'public_key', $public_key );
		$seed->update_option( 'secret_token', 'sk_test_secret' );
		$seed->update_option( 'connection_version', $version );

		return new WC_Gateway_Monokulo();
	}

	/**
	 * A canned Monokulo `POST /pay/{pk}/orders` error response.
	 *
	 * @param int    $code  HTTP status.
	 * @param string $error Monokulo's JSON `error` message.
	 * @return array
	 */
	private function error_response( $code, $error ) {
		return array(
			'headers'  => array(),
			'body'     => wp_json_encode( array( 'error' => $error ) ),
			'response' => array( 'code' => $code, 'message' => 'Error' ),
			'cookies'  => array(),
		);
	}

	/**
	 * Registers a `pre_http_request` callback that records the single
	 * request it observes into `$this->captured_request` and short-circuits
	 * it with the given canned response array.
	 *
	 * @param array $canned_response The `pre_http_request`-shaped response to
	 *                                return (see this file's own class doc
	 *                                comment for the shape WordPress expects).
	 */
	private function mock_next_http_response( array $canned_response ) {
		add_filter(
			'pre_http_request',
			function ( $preempt, $args, $url ) use ( $canned_response ) {
				$this->captured_request = array(
					'url'  => $url,
					'args' => $args,
				);
				return $canned_response;
			},
			10,
			3
		);
	}

	/**
	 * Placing a real order through this gateway sends the exact request
	 * (URL, method, secret-key header, JSON body) to Monokulo's
	 * `POST /pay/{pk}/orders`, and returns Monokulo's checkout page for the
	 * new order as the redirect WooCommerce's own checkout JS
	 * (`WC_Checkout::process_order_payment()`) sends the customer's browser to.
	 */
	public function test_process_payment_sends_expected_request_and_returns_monokulo_checkout_redirect() {
		$order    = $this->create_real_order();
		$gateway  = $this->create_configured_gateway( 'http://monokulo.test/', 'pk_test_abc123' );

		$this->mock_next_http_response(
			array(
				'headers'  => array(),
				'body'     => wp_json_encode(
					array(
						'order_id'           => 'pay_deadbeef',
						'address'              => '4Axxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx',
						'xmr_amount_piconero'  => 123456789012,
						'amount'               => '42.50',
						'currency'             => 'USD',
						'merchant_order_id'    => (string) $order->get_id(),
						'expires_at'           => 4102444800,
					)
				),
				'response' => array(
					'code'    => 200,
					'message' => 'OK',
				),
				'cookies'  => array(),
			)
		);

		$result = $gateway->process_payment( $order->get_id() );

		// --- The request this gateway actually sent, asserted in full. ---
		$this->assertNotNull( $this->captured_request, 'process_payment() should have made exactly one HTTP call.' );
		$this->assertSame(
			'http://monokulo.test/pay/pk_test_abc123/orders',
			$this->captured_request['url'],
			'Should POST to Monokulo\'s order-creation route (crates/monokulo/src/http/pay.rs), never the engine.'
		);
		$this->assertSame( 'POST', $this->captured_request['args']['method'] );
		$this->assertSame( 'application/json', $this->captured_request['args']['headers']['Content-Type'] );
		$this->assertSame(
			'Bearer sk_test_secret',
			$this->captured_request['args']['headers']['Authorization'],
			'Orders are created with the store\'s secret key, so Monokulo knows they come from the shop\'s server.'
		);

		$sent_body = json_decode( $this->captured_request['args']['body'], true );
		$this->assertIsArray( $sent_body, 'Request body should be valid JSON.' );
		$this->assertSame(
			array( 'amount', 'currency', 'merchant_order_id' ),
			array_keys( $sent_body ),
			'Exactly the shape of Monokulo\'s CreateOrderRequest.'
		);
		$this->assertSame(
			'42.50',
			$sent_body['amount'],
			'amount must be a plain two-decimal-place decimal string, not a locale-formatted number.'
		);
		$this->assertSame( 'USD', $sent_body['currency'] );
		$this->assertSame( (string) $order->get_id(), $sent_body['merchant_order_id'] );

		// --- The redirect WooCommerce receives back. ---
		$this->assertSame( 'success', $result['result'] );
		$this->assertSame(
			'http://monokulo.test/pay/pk_test_abc123/orders/pay_deadbeef',
			$result['redirect'],
			'Should redirect to Monokulo\'s checkout page for the new order - not WooCommerce\'s own ' .
			'get_return_url() thank-you page, since no payment has actually happened yet.'
		);

		// The one fact 1.5.4's webhook receiver will need later, recorded now.
		$order = wc_get_order( $order->get_id() );
		$this->assertSame( 'pay_deadbeef', $order->get_meta( '_monokulo_order_id' ) );
	}

	/**
	 * A gateway with no connection yet (the out-of-the-box state) must fail
	 * loudly, not silently call `wp_remote_post()` against an empty URL.
	 */
	public function test_process_payment_throws_when_gateway_is_not_configured() {
		$order   = $this->create_real_order();
		$gateway = new WC_Gateway_Monokulo(); // Fresh install: endpoint/public_key both default to ''.

		$this->mock_next_http_response(
			array(
				'headers'  => array(),
				'body'     => '',
				'response' => array( 'code' => 200, 'message' => 'OK' ),
				'cookies'  => array(),
			)
		);

		$this->expectException( Exception::class );
		$gateway->process_payment( $order->get_id() );
	}

	/**
	 * A non-200 Monokulo response (e.g. an unsupported currency) must
	 * surface as a thrown `Exception`, per `process_payment()`'s own
	 * documented failure-signaling contract, not as a silently-accepted
	 * "success" with a garbage redirect.
	 */
	public function test_process_payment_throws_when_monokulo_returns_non_200() {
		$order   = $this->create_real_order();
		$gateway = $this->create_configured_gateway();

		$this->mock_next_http_response(
			array(
				'headers'  => array(),
				'body'     => wp_json_encode( array( 'error' => 'unsupported currency: USD' ) ),
				'response' => array( 'code' => 400, 'message' => 'Bad Request' ),
				'cookies'  => array(),
			)
		);

		$this->expectException( Exception::class );
		$gateway->process_payment( $order->get_id() );
	}

	/**
	 * A transport-level failure (DNS, connection refused, timeout - anything
	 * `WP_Http` itself represents as a `WP_Error` rather than an HTTP
	 * response) must also surface as a thrown `Exception`, exercised here by
	 * having the mocked filter return a `WP_Error` directly - exactly what
	 * `pre_http_request` returning a `WP_Error` does in `WP_Http::request()`
	 * (read directly: it's returned to the caller unchanged, and
	 * `wp_remote_post()` callers are expected to check `is_wp_error()` before
	 * touching the result, which `create_engine_order()` does).
	 */
	public function test_process_payment_throws_when_monokulo_is_unreachable() {
		$order   = $this->create_real_order();
		$gateway = $this->create_configured_gateway();

		add_filter(
			'pre_http_request',
			function () {
				return new WP_Error( 'http_request_failed', 'cURL error 7: Failed to connect to monokulo.test port 80' );
			}
		);

		$this->expectException( Exception::class );
		$gateway->process_payment( $order->get_id() );
	}

	/**
	 * Monokulo's 401 (key rejected), 403 (refused by the store's policy) and
	 * 429 (rate limited) each become a customer-safe message: the order
	 * isn't marked as anything, and nothing of Monokulo's own error text
	 * reaches the customer.
	 */
	public function test_process_payment_turns_401_403_and_429_into_customer_safe_messages() {
		$cases = array(
			401 => 'not available for this store',
			403 => 'not available for this store',
			429 => 'busy right now',
		);
		foreach ( $cases as $code => $expected ) {
			remove_all_filters( 'pre_http_request' );
			$order   = $this->create_real_order();
			$gateway = $this->create_configured_gateway();
			$this->mock_next_http_response( $this->error_response( $code, 'internal detail ' . $code ) );

			try {
				$gateway->process_payment( $order->get_id() );
				$this->fail( "HTTP $code should have thrown." );
			} catch ( Exception $e ) {
				$this->assertStringContainsString( $expected, $e->getMessage(), "HTTP $code" );
				$this->assertStringNotContainsString( 'internal detail', $e->getMessage(), 'Monokulo\'s own error text is logged, not shown.' );
			}
			$this->assertSame( '', wc_get_order( $order->get_id() )->get_meta( '_monokulo_order_id' ) );
		}
	}

	/**
	 * Amounts go to Monokulo as plain decimal strings: two decimal places
	 * for fiat (Monokulo refuses more), and XMR's own precision (the store's
	 * price decimals, up to twelve) for an XMR-priced store. Called through
	 * reflection because WooCommerce caches its currency list, so a test
	 * can't add XMR to it after the fact to build a real XMR order.
	 */
	public function test_amounts_are_plain_decimals_with_the_right_precision() {
		$format = new ReflectionMethod( WC_Gateway_Monokulo::class, 'format_amount' );
		$format->setAccessible( true );

		$this->assertSame( '42.50', $format->invoke( null, '42.5', 'USD' ) );
		$this->assertSame( '1234567.00', $format->invoke( null, 1234567, 'EUR' ), 'No thousands separator.' );
		update_option( 'woocommerce_price_num_decimals', 6 );
		$this->assertSame( '0.123456', $format->invoke( null, '0.123456', 'XMR' ) );
		$this->assertSame( '0.123456', $format->invoke( null, '0.123456', 'xmr' ) );
	}

	/**
	 * An install connected by an older plugin version (credentials, but no
	 * `connection_version`) holds the engine's address, which no longer
	 * takes orders. It must be hidden at checkout, refuse orders without
	 * calling anything, and show the reconnect notice - recognised by the
	 * missing marker, never by the URL.
	 */
	public function test_an_install_connected_before_this_version_must_reconnect() {
		$gateway = $this->create_configured_gateway( 'http://monokulo.test', 'pk_test_abc123', '' );

		$this->assertTrue( $gateway->needs_reconnect() );
		$this->assertFalse( $gateway->is_available(), 'Hidden at checkout until reconnected, rather than failing orders.' );

		$order = $this->create_real_order();
		try {
			$gateway->process_payment( $order->get_id() );
			$this->fail( 'An old connection must not create orders.' );
		} catch ( Exception $e ) {
			$this->assertNull( $this->captured_request, 'Nothing is sent for an old connection.' );
		}

		wp_set_current_user( self::factory()->user->create( array( 'role' => 'administrator' ) ) );
		ob_start();
		WC_Gateway_Monokulo::render_reconnect_notice();
		$notice = ob_get_clean();
		$this->assertStringContainsString( 'please reconnect', $notice );
		$this->assertStringContainsString( 'section=monokulo', $notice );

		// A current connection: available, and no notice.
		$current = $this->create_configured_gateway();
		$this->assertFalse( $current->needs_reconnect() );
		$this->assertTrue( $current->is_available() );
		ob_start();
		WC_Gateway_Monokulo::render_reconnect_notice();
		$this->assertSame( '', ob_get_clean() );
	}

	/**
	 * A store that never connected isn't told to "reconnect" - it just
	 * isn't connected yet.
	 */
	public function test_a_store_that_never_connected_gets_no_reconnect_notice() {
		$this->assertFalse( WC_Gateway_Monokulo::settings_need_reconnect( false ) );
		$this->assertFalse( WC_Gateway_Monokulo::settings_need_reconnect( array( 'enabled' => 'no', 'endpoint' => '' ) ) );
		$this->assertTrue( WC_Gateway_Monokulo::settings_need_reconnect( array( 'endpoint' => 'http://anything' ) ) );
	}
}
