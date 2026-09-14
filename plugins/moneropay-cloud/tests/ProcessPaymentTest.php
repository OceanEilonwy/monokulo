<?php
/**
 * WBS 1.5.2's acceptance test: `process_payment()` actually calls the real
 * engine order-creation API with the request WooCommerce's own real checkout
 * flow would have produced, and hands WooCommerce back the real
 * `/pay/v1/{pk}/{payment_id}` redirect shape.
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
 * @package MoneroPayCloud
 */

/**
 * Asserts `WC_Gateway_MoneroPay::process_payment()` against a real `WC_Order`
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
	 * Builds a `WC_Gateway_MoneroPay` with the 1.5.3-stand-in `endpoint`/
	 * `public_key` settings fields (see that class's own doc comment on
	 * `$api_base_url`) already configured - `update_option()` persists to
	 * `wp_options` immediately (`WC_Settings_API::update_option()`, checked
	 * directly), so a *second*, freshly-constructed gateway object picks the
	 * values back up through its own constructor's normal
	 * `init_settings()`/`get_option()` path, exactly like a real merchant's
	 * saved settings would on the next request.
	 *
	 * @param string $endpoint   The engine base URL to configure.
	 * @param string $public_key The tenant public key to configure.
	 * @return WC_Gateway_MoneroPay
	 */
	private function create_configured_gateway( $endpoint = 'http://engine.test', $public_key = 'pk_test_abc123' ) {
		$seed = new WC_Gateway_MoneroPay();
		$seed->update_option( 'endpoint', $endpoint );
		$seed->update_option( 'public_key', $public_key );

		return new WC_Gateway_MoneroPay();
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
	 * The core WBS 1.5.2 acceptance assertion: placing a real order through
	 * this gateway sends the exact request WooCommerce's real checkout would
	 * have sent (URL, method, JSON body) to the engine's real
	 * `POST /api/v1/t/{pk}/orders`, and returns the exact
	 * `/pay/v1/{pk}/{payment_id}` redirect WooCommerce's own checkout JS
	 * (`WC_Checkout::process_order_payment()`, read directly - see this
	 * class's file-level doc comment) sends the customer's browser to.
	 */
	public function test_process_payment_sends_expected_request_and_returns_engine_redirect() {
		$order    = $this->create_real_order();
		$gateway  = $this->create_configured_gateway( 'http://engine.test', 'pk_test_abc123' );

		$this->mock_next_http_response(
			array(
				'headers'  => array(),
				'body'     => wp_json_encode(
					array(
						'payment_id'           => 'pay_deadbeef',
						'address'              => '4Axxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx',
						'xmr_amount_piconero'  => 123456789012,
						'fiat_amount'          => '42.50',
						'fiat_currency'        => 'USD',
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
			'http://engine.test/api/v1/t/pk_test_abc123/orders',
			$this->captured_request['url'],
			'Should POST to the real public order-creation route (src/http/mod.rs), keyed by the ' .
			'configured tenant public key, never a secret token.'
		);
		$this->assertSame( 'POST', $this->captured_request['args']['method'] );
		$this->assertSame(
			'application/json',
			$this->captured_request['args']['headers']['Content-Type'],
			'The engine\'s create_order handler expects a Json<CreateOrderRequest> body (src/http/public.rs).'
		);

		$sent_body = json_decode( $this->captured_request['args']['body'], true );
		$this->assertIsArray( $sent_body, 'Request body should be valid JSON.' );
		$this->assertSame(
			'42.50',
			$sent_body['fiat_amount'],
			'fiat_amount must be a plain two-decimal-place decimal string - exactly what ' .
			'exchange_rate::compute_xmr_amount() (src/exchange_rate.rs) parses, not a locale-formatted number.'
		);
		$this->assertSame( 'USD', $sent_body['fiat_currency'] );
		$this->assertSame( (string) $order->get_id(), $sent_body['merchant_order_id'] );
		$this->assertArrayHasKey( 'description', $sent_body );

		// --- The redirect WooCommerce receives back, asserted against the ---
		// --- real /pay/v1/{pk}/{payment_id} shape the mocked response's   ---
		// --- own payment_id implies.                                     ---
		$this->assertSame( 'success', $result['result'] );
		$this->assertSame(
			'http://engine.test/pay/v1/pk_test_abc123/pay_deadbeef',
			$result['redirect'],
			'Should redirect to the engine\'s own already-built checkout page for the payment_id the ' .
			'canned order-creation response returned - not WooCommerce\'s own get_return_url() thank-you ' .
			'page, since no payment has actually happened yet.'
		);

		// The one fact 1.5.4's webhook receiver will need later, recorded now.
		$order = wc_get_order( $order->get_id() );
		$this->assertSame( 'pay_deadbeef', $order->get_meta( '_moneropay_cloud_payment_id' ) );
	}

	/**
	 * A gateway with no `endpoint`/`public_key` configured yet (the real
	 * out-of-the-box state before a merchant fills in the 1.5.3-stand-in
	 * fields) must fail loudly, not silently call `wp_remote_post()` against
	 * an empty URL - proves `create_engine_order()`'s own guard clause is
	 * real, exercised behavior, not just a comment's claim about it.
	 */
	public function test_process_payment_throws_when_gateway_is_not_configured() {
		$order   = $this->create_real_order();
		$gateway = new WC_Gateway_MoneroPay(); // Fresh install: endpoint/public_key both default to ''.

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
	 * A non-200 engine response (e.g. the tenant's public key doesn't exist,
	 * or the requested currency has no configured exchange rate - both real
	 * `ApiError` cases in `src/http/public.rs::create_order`) must surface as
	 * a thrown `Exception`, per `process_payment()`'s own documented
	 * failure-signaling contract, not as a silently-accepted "success" with a
	 * garbage redirect.
	 */
	public function test_process_payment_throws_when_engine_returns_non_200() {
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
	public function test_process_payment_throws_when_the_engine_is_unreachable() {
		$order   = $this->create_real_order();
		$gateway = $this->create_configured_gateway();

		add_filter(
			'pre_http_request',
			function () {
				return new WP_Error( 'http_request_failed', 'cURL error 7: Failed to connect to engine.test port 80' );
			}
		);

		$this->expectException( Exception::class );
		$gateway->process_payment( $order->get_id() );
	}
}
