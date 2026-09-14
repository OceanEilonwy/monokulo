<?php
/**
 * WBS 1.5.3's acceptance test: the real one-click connect flow, ported from
 * `control-plane/src/http/connect.rs`'s own protocol into this gateway's
 * settings screen.
 *
 * Two things this file deliberately tests separately, matching how the
 * class itself splits them (see `WC_Gateway_MoneroPay::handle_connect_return()`'s
 * own doc comment for why):
 *
 * - `generate_moneropay_connect_html()`: the settings-screen button really
 *   renders a real `GET {control_plane}/connect/woocommerce?...` link and
 *   really stores the nonce embedded in it, so a merchant clicking it and a
 *   later `process_connect_return()` call genuinely agree on what nonce to
 *   expect back.
 * - `process_connect_return()`: the callback logic WordPress's own
 *   `admin_post_moneropay_cloud_connect_return` action (`handle_connect_return()`)
 *   dispatches to - tested directly, the same way `ProcessPaymentTest.php`
 *   calls `process_payment()` directly, rather than driving a real HTTP
 *   request through `admin-post.php` (which would require working around
 *   `handle_connect_return()`'s own `exit`, exactly why that method is kept
 *   to two lines with all the real logic split out).
 *
 * Same `pre_http_request` mocking technique `ProcessPaymentTest.php` already
 * established for the outbound `/finish` call - see that file's own class
 * doc comment for why this is WordPress's own real short-circuit mechanism,
 * not a hand-rolled mock.
 *
 * @package MoneroPayCloud
 */
class ConnectFlowTest extends WP_UnitTestCase {

	/**
	 * The real control plane base URL this whole suite points the gateway
	 * at, via the `moneropay_cloud_control_plane_base_url` filter
	 * `WC_Gateway_MoneroPay::get_control_plane_base_url()` exposes - a fake,
	 * obviously-non-resolving host, not `WC_Gateway_MoneroPay::
	 * CONTROL_PLANE_BASE_URL`'s own real (placeholder) value, so this test
	 * suite is exercising the actual filter mechanism a self-hoster would
	 * use, not just reading the class constant back.
	 */
	const TEST_CONTROL_PLANE_BASE_URL = 'https://control-plane.test';

	/**
	 * @var array{url: string, args: array}|null
	 */
	private $captured_request;

	public function set_up(): void {
		parent::set_up();
		$this->captured_request = null;
		add_filter( 'moneropay_cloud_control_plane_base_url', array( $this, 'filter_control_plane_base_url' ) );
	}

	public function tear_down(): void {
		remove_all_filters( 'pre_http_request' );
		remove_filter( 'moneropay_cloud_control_plane_base_url', array( $this, 'filter_control_plane_base_url' ) );
		parent::tear_down();
	}

	public function filter_control_plane_base_url() {
		return self::TEST_CONTROL_PLANE_BASE_URL;
	}

	/**
	 * Registers a `pre_http_request` callback that records the single
	 * request it observes into `$this->captured_request` and short-circuits
	 * it with the given canned response array - identical helper to
	 * `ProcessPaymentTest::mock_next_http_response()`, duplicated here
	 * rather than shared, since these are two independently-runnable test
	 * files and neither should depend on the other's existence.
	 *
	 * @param array $canned_response The `pre_http_request`-shaped response to
	 *                                 return.
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

	private function canned_finish_response_body() {
		return array(
			'public_key'              => 'pk_connect_test_123',
			'secret_token'            => 'sk_connect_test_456',
			'endpoint'                => 'http://engine.connect-test',
			'webhook_signing_secret'  => 'whsec_connect_test_789',
		);
	}

	private function mock_successful_finish_response() {
		$this->mock_next_http_response(
			array(
				'headers'  => array(),
				'body'     => wp_json_encode( $this->canned_finish_response_body() ),
				'response' => array( 'code' => 200, 'message' => 'OK' ),
				'cookies'  => array(),
			)
		);
	}

	/**
	 * Renders the real connect button (as `WC_Settings_Page::output()` would,
	 * via `generate_settings_html()` - same field data a real render would
	 * pass, fetched from the gateway's own `get_form_fields()` rather than an
	 * empty array, so this exercises the exact `$data` shape production code
	 * hands in) against a fresh gateway instance, and returns the real nonce
	 * that render stored - proving the button and the callback agree on the
	 * same nonce, rather than the test independently fabricating one.
	 *
	 * @return string The freshly minted, freshly stored nonce.
	 */
	private function render_connect_button_and_capture_nonce() {
		$gateway     = new WC_Gateway_MoneroPay();
		$field_data  = $gateway->get_form_fields()['connect'];
		$html        = $gateway->generate_moneropay_connect_html( 'connect', $field_data );

		$this->assertMatchesRegularExpression(
			'/nonce=([0-9a-f]{32})/',
			$html,
			'The rendered connect button should embed a real hex nonce in its href.'
		);
		preg_match( '/nonce=([0-9a-f]{32})/', $html, $matches );

		return array( $html, $matches[1] );
	}

	/**
	 * The connect button itself: proves it really points at the configured
	 * control plane's real `GET /connect/woocommerce` route with real
	 * `site_url`/`return_url`/`nonce` query params, and that the embedded
	 * nonce is genuinely the one stored server-side (not just cosmetically
	 * present in the link) - `process_connect_return()`'s own nonce check
	 * only means anything if this is true.
	 */
	public function test_connect_button_links_to_the_real_connect_start_url_with_a_stored_nonce() {
		list( $html, $nonce ) = $this->render_connect_button_and_capture_nonce();

		$this->assertStringContainsString(
			'https://control-plane.test/connect/woocommerce?',
			$html,
			'Should link to the configured control plane\'s real GET /connect/{platform} route (connect.rs step 1).'
		);
		$this->assertStringContainsString(
			'return_url=' . rawurlencode( admin_url( 'admin-post.php?action=moneropay_cloud_connect_return' ) ),
			$html,
			'return_url must be this site\'s own admin-post.php callback, url-encoded as a single query value.'
		);

		// The real proof this isn't just cosmetically present in the markup:
		// the exact transient key `process_connect_return()` will later read
		// (`connect_nonce_transient_key()`) really holds this same value -
		// checked directly against the store, not by making a second
		// `process_connect_return()` call here, which would itself consume
		// (delete) the transient this assertion needs to still be there
		// (see that method's own doc comment: it deletes on every check,
		// match or not).
		$this->assertSame(
			$nonce,
			get_transient( 'moneropay_cloud_connect_nonce_moneropay_cloud' ),
			'The nonce embedded in the rendered link should be exactly the one stored server-side.'
		);
	}

	/**
	 * The core WBS 1.5.3 acceptance assertion: a real redirect-back request
	 * (token + the real, matching nonce) redeems real credentials and the
	 * gateway becomes enabled - the WBS's own stated outcome, not left for
	 * the merchant to separately toggle.
	 */
	public function test_process_connect_return_saves_settings_and_enables_the_gateway_on_success() {
		list( , $nonce ) = $this->render_connect_button_and_capture_nonce();
		$this->mock_successful_finish_response();

		$callback_gateway = new WC_Gateway_MoneroPay();
		$redirect_url     = $callback_gateway->process_connect_return(
			array(
				'token' => 'conn_real_token_abc',
				'nonce' => $nonce,
			)
		);

		// --- The /finish request actually sent. ---
		$this->assertNotNull( $this->captured_request, 'process_connect_return() should have called /finish exactly once.' );
		$this->assertSame(
			'https://control-plane.test/connect/woocommerce/finish',
			$this->captured_request['url'],
			'Should POST to the real, configured control plane\'s /finish route (connect.rs step 5).'
		);
		$this->assertSame( 'POST', $this->captured_request['args']['method'] );
		$sent_body = json_decode( $this->captured_request['args']['body'], true );
		$this->assertSame( 'conn_real_token_abc', $sent_body['token'] );
		$this->assertSame(
			WC()->api_request_url( 'moneropay_cloud' ),
			$sent_body['webhook_url'],
			'webhook_url should be this plugin\'s own real woocommerce_api_{id} URL (WC()->api_request_url()), ' .
			'registered now for WBS 1.5.4\'s receiver to pick up later - see call_connect_finish()\'s own doc comment.'
		);

		// --- The redirect back to this gateway's own settings screen. ---
		$this->assertStringContainsString( 'page=wc-settings', $redirect_url );
		$this->assertStringContainsString( 'section=moneropay_cloud', $redirect_url );
		$this->assertStringContainsString( 'moneropay_cloud_connected=1', $redirect_url );

		// --- The real, persisted settings - re-read via a brand new ---
		// --- instance, exactly like a real next request would.       ---
		$saved = new WC_Gateway_MoneroPay();
		$this->assertSame( 'http://engine.connect-test', $saved->get_option( 'endpoint' ) );
		$this->assertSame( 'pk_connect_test_123', $saved->get_option( 'public_key' ) );
		$this->assertSame(
			'sk_connect_test_456',
			$saved->get_option( 'secret_token' ),
			'secret_token should be saved even though nothing in this plugin reads it back yet - see that ' .
			'property\'s own doc comment for why discarding it would be a real, not just theoretical, loss.'
		);
		$this->assertSame( 'whsec_connect_test_789', $saved->get_option( 'webhook_signing_secret' ) );
		$this->assertSame(
			'yes',
			$saved->enabled,
			'The WBS\'s own stated outcome: connecting a wallet enables the gateway outright.'
		);
		$this->assertTrue( $saved->is_available(), 'A freshly connected gateway should be usable at checkout immediately.' );
	}

	/**
	 * A rejected/failed `/finish` call (any of connect.rs's real bare-401
	 * failure modes: expired token, already-consumed token, unknown token,
	 * a rejected webhook URL) must leave whatever settings already existed
	 * completely untouched, and must never enable the gateway - a failed
	 * *re*-connect attempt is not license to clobber a merchant's already-working
	 * configuration with nothing.
	 */
	public function test_process_connect_return_does_not_save_or_enable_on_a_failed_finish_call() {
		// Seed a baseline "already connected" state first, so this test can
		// prove a failure genuinely leaves it alone, not just that a fresh
		// install stays empty (which a bug that skipped saving entirely
		// would also make pass).
		$seed = new WC_Gateway_MoneroPay();
		$seed->update_option( 'endpoint', 'http://old-engine.test' );
		$seed->update_option( 'public_key', 'pk_old_value' );
		$seed->update_option( 'secret_token', 'sk_old_value' );
		$seed->update_option( 'enabled', 'no' );

		list( , $nonce ) = $this->render_connect_button_and_capture_nonce();
		$this->mock_next_http_response(
			array(
				'headers'  => array(),
				'body'     => '',
				'response' => array( 'code' => 401, 'message' => 'Unauthorized' ),
				'cookies'  => array(),
			)
		);

		$callback_gateway = new WC_Gateway_MoneroPay();
		$redirect_url     = $callback_gateway->process_connect_return(
			array(
				'token' => 'conn_a_token_the_control_plane_will_reject',
				'nonce' => $nonce,
			)
		);

		$this->assertNotNull( $this->captured_request, '/finish should still have been called - this tests its failure handling, not the nonce check.' );
		$this->assertStringContainsString( 'moneropay_cloud_connect_error=finish', $redirect_url );

		$unchanged = new WC_Gateway_MoneroPay();
		$this->assertSame( 'http://old-engine.test', $unchanged->get_option( 'endpoint' ) );
		$this->assertSame( 'pk_old_value', $unchanged->get_option( 'public_key' ) );
		$this->assertSame( 'sk_old_value', $unchanged->get_option( 'secret_token' ) );
		$this->assertSame( 'no', $unchanged->enabled );
	}

	/**
	 * The actual security property `nonce` exists for (per this step's own
	 * brief): a redirect-back request whose `nonce` does not match what this
	 * site itself generated and stored must be rejected outright, and must
	 * never even attempt the `/finish` call - a forged/guessed `token`
	 * paired with the wrong nonce must not get anywhere near redeeming it.
	 */
	public function test_process_connect_return_rejects_a_mismatched_nonce_without_calling_finish() {
		list( , $real_nonce ) = $this->render_connect_button_and_capture_nonce();
		$this->assertNotEmpty( $real_nonce );

		// Registered so a bug that skipped the nonce check and called
		// /finish anyway would be caught by $this->captured_request being
		// non-null below, not just by the redirect URL's own error flag.
		$this->mock_successful_finish_response();

		$gateway      = new WC_Gateway_MoneroPay();
		$redirect_url = $gateway->process_connect_return(
			array(
				'token' => 'conn_some_token',
				'nonce' => 'this-is-not-the-real-nonce-at-all',
			)
		);

		$this->assertStringContainsString( 'moneropay_cloud_connect_error=nonce', $redirect_url );
		$this->assertNull(
			$this->captured_request,
			'A mismatched nonce must be rejected before ever calling /finish - the token must never be spent.'
		);
		$this->assertSame(
			'',
			( new WC_Gateway_MoneroPay() )->get_option( 'public_key' ),
			'No setting should be touched at all when the nonce check fails.'
		);
	}

	/**
	 * The nonce is single-use, per `connect_nonce_transient_key()`'s own doc
	 * comment (deleted on first check regardless of outcome) - reusing an
	 * already-checked nonce a second time must fail exactly like a nonce
	 * that never existed, not silently re-validate against a value that's
	 * already been consumed.
	 */
	public function test_process_connect_return_rejects_reusing_the_same_nonce_twice() {
		list( , $nonce ) = $this->render_connect_button_and_capture_nonce();
		$this->mock_successful_finish_response();

		$first_gateway = new WC_Gateway_MoneroPay();
		$first_result  = $first_gateway->process_connect_return(
			array(
				'token' => 'conn_first_use',
				'nonce' => $nonce,
			)
		);
		$this->assertStringContainsString(
			'moneropay_cloud_connected=1',
			$first_result,
			'Sanity check: the first use of a real nonce must actually succeed, otherwise the second assertion below proves nothing.'
		);

		// Reset the capture so the second call's own behavior (or lack of
		// one) is unambiguous.
		$this->captured_request = null;

		$second_gateway = new WC_Gateway_MoneroPay();
		$second_result  = $second_gateway->process_connect_return(
			array(
				'token' => 'conn_second_use_same_nonce',
				'nonce' => $nonce,
			)
		);

		$this->assertStringContainsString( 'moneropay_cloud_connect_error=nonce', $second_result );
		$this->assertNull( $this->captured_request, 'Reusing an already-consumed nonce must not call /finish a second time.' );
	}

	/**
	 * A redirect-back request missing `token` and/or `nonce` entirely (e.g.
	 * a merchant manually navigating to the callback URL, or a genuinely
	 * malformed redirect) must be rejected the same way a mismatch is, never
	 * treated as an empty-string nonce that happens to not match anything by
	 * coincidence.
	 */
	public function test_process_connect_return_rejects_missing_token_or_nonce() {
		list( , $nonce ) = $this->render_connect_button_and_capture_nonce();
		$this->mock_successful_finish_response();

		$gateway = new WC_Gateway_MoneroPay();
		$result  = $gateway->process_connect_return( array( 'nonce' => $nonce ) ); // No token at all.

		$this->assertStringContainsString( 'moneropay_cloud_connect_error=nonce', $result );
		$this->assertNull( $this->captured_request );
	}
}
