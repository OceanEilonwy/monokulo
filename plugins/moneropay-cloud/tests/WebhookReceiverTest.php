<?php
/**
 * WBS 1.5.4's request-handling acceptance tests: the real behavior of
 * `process_webhook_request()` (the logic behind the `woocommerce_api_{id}`
 * hook target, `handle_webhook()`) against real, correctly- and
 * incorrectly-signed request bodies and a real `WC_Order`.
 *
 * **Driven directly through `process_webhook_request( $raw_body, $signature )`,
 * not through a real HTTP request to `?wc-api=moneropay_cloud`** - the same
 * pattern `ConnectFlowTest.php` already established for `process_connect_
 * return()` and `ProcessPaymentTest.php` for `process_payment()`: this
 * plugin's convention throughout is to call the real, side-effect-bearing
 * method directly with a hand-built input, rather than driving a full
 * request through WordPress's own router (`handle_webhook()`'s own doc
 * comment explains why that thin wrapper is deliberately *not* the unit
 * under test here - it reads `php://input`/`$_SERVER` and calls `exit`,
 * neither of which a PHPUnit process can exercise directly). This is real
 * coverage of every decision `process_webhook_request()` makes; only the
 * `file_get_contents( 'php://input' )` / `$_SERVER` plumbing itself is
 * untested by this file - and that plumbing is two lines with nothing
 * meaningful to assert beyond "it reads the right superglobal", confirmed by
 * inspection, the same judgment call `ConnectFlowTest.php`'s own class doc
 * comment already makes about `handle_connect_return()`'s two-line wrapper.
 *
 * @package MoneroPayCloud
 */
class WebhookReceiverTest extends WP_UnitTestCase {

	const SECRET = 'whsec_receiver_test_secret';

	/**
	 * Builds a real, saved `WC_Order` already carrying the
	 * `_moneropay_cloud_payment_id` meta `process_payment()` would have
	 * written - the exact fact `find_order_by_payment_id()` looks up.
	 *
	 * @param string $payment_id
	 * @param string $status Initial WC order status.
	 * @return WC_Order
	 */
	private function create_order_for_payment_id( $payment_id, $status = 'pending' ) {
		$order = wc_create_order();
		$order->set_status( $status );
		$order->update_meta_data( WC_Gateway_MoneroPay::META_PAYMENT_ID, $payment_id );
		$order->save();
		return $order;
	}

	/**
	 * @return WC_Gateway_MoneroPay A gateway configured with `self::SECRET`
	 *                               as its webhook_signing_secret.
	 */
	private function create_gateway() {
		$seed = new WC_Gateway_MoneroPay();
		$seed->update_option( 'webhook_signing_secret', self::SECRET );
		return new WC_Gateway_MoneroPay();
	}

	/**
	 * Builds a real event envelope exactly as `enqueue_webhook_event()` in
	 * `src/scanner.rs` shapes one, and signs it exactly as `sign_payload()`
	 * in `shared/src/webhook_sign.rs` does - hex HMAC-SHA256 of the raw JSON
	 * bytes. Returns both the raw body and its real signature, so a test can
	 * corrupt either independently.
	 *
	 * @param array  $fields Event-specific fields (plus event/event_id/payment_id,
	 *                        which callers may override).
	 * @param string $secret Signing key - defaults to `self::SECRET`, overridable
	 *                        so a test can prove a wrong-secret signature is
	 *                        rejected.
	 * @return array{0: string, 1: string} [$raw_body, $signature]
	 */
	private function build_signed_event( array $fields, $secret = self::SECRET ) {
		$body = wp_json_encode(
			array_merge(
				array(
					'event_id'   => 'evt_' . wp_generate_password( 12, false ),
					'created_at' => time(),
				),
				$fields
			)
		);
		return array( $body, hash_hmac( 'sha256', $body, $secret ) );
	}

	// --- Signature verification, at the request-handling level. ------------

	public function test_correctly_signed_known_event_returns_200_and_processes_it() {
		$order = $this->create_order_for_payment_id( 'pay_receiver_ok' );
		list( $body, $sig ) = $this->build_signed_event(
			array( 'event' => 'order.paid', 'payment_id' => 'pay_receiver_ok', 'status' => 'paid' )
		);

		$status_code = $this->create_gateway()->process_webhook_request( $body, $sig );

		$this->assertSame( 200, $status_code );
		$order = wc_get_order( $order->get_id() );
		$this->assertTrue( $order->has_status( array( 'processing', 'completed' ) ), 'order.paid should mark the order paid via payment_complete().' );
	}

	public function test_missing_signature_is_rejected_and_the_order_is_left_untouched() {
		$order = $this->create_order_for_payment_id( 'pay_receiver_nosig', 'pending' );
		list( $body, $sig ) = $this->build_signed_event(
			array( 'event' => 'order.paid', 'payment_id' => 'pay_receiver_nosig', 'status' => 'paid' )
		);

		$status_code = $this->create_gateway()->process_webhook_request( $body, '' );

		$this->assertSame( 401, $status_code );
		$this->assertSame( 'pending', wc_get_order( $order->get_id() )->get_status(), 'A rejected request must never touch the order.' );
	}

	public function test_invalid_signature_is_rejected_and_the_order_is_left_untouched() {
		$order = $this->create_order_for_payment_id( 'pay_receiver_badsig', 'pending' );
		list( $body, $real_sig ) = $this->build_signed_event(
			array( 'event' => 'order.paid', 'payment_id' => 'pay_receiver_badsig', 'status' => 'paid' )
		);
		// Flip one hex character - a near-miss, exactly the input a
		// non-constant-time comparison would take longest to reject.
		$tampered_sig = substr( $real_sig, 0, -1 ) . ( '0' === substr( $real_sig, -1 ) ? '1' : '0' );

		$status_code = $this->create_gateway()->process_webhook_request( $body, $tampered_sig );

		$this->assertSame( 401, $status_code );
		$this->assertSame( 'pending', wc_get_order( $order->get_id() )->get_status() );
	}

	public function test_signature_valid_under_a_different_secret_is_rejected() {
		// Proves the check is genuinely keyed by *this* gateway's own
		// configured secret, not merely "is this valid hex of the right
		// length" - a signature that is perfectly well-formed, just signed
		// with the wrong key.
		$order = $this->create_order_for_payment_id( 'pay_receiver_wrongkey', 'pending' );
		list( $body, $sig ) = $this->build_signed_event(
			array( 'event' => 'order.paid', 'payment_id' => 'pay_receiver_wrongkey', 'status' => 'paid' ),
			'a-completely-different-secret'
		);

		$status_code = $this->create_gateway()->process_webhook_request( $body, $sig );

		$this->assertSame( 401, $status_code );
		$this->assertSame( 'pending', wc_get_order( $order->get_id() )->get_status() );
	}

	// --- Envelope validation. ------------------------------------------------

	public function test_correctly_signed_but_malformed_body_is_rejected_as_bad_request() {
		$body = 'this is not json at all';
		$sig  = hash_hmac( 'sha256', $body, self::SECRET );

		$status_code = $this->create_gateway()->process_webhook_request( $body, $sig );

		$this->assertSame( 400, $status_code, 'A correctly-signed but unparseable body is a client error, not an auth or lookup failure.' );
	}

	public function test_correctly_signed_body_missing_required_envelope_fields_is_rejected_as_bad_request() {
		$body = wp_json_encode( array( 'event' => 'order.paid' ) ); // no event_id, no payment_id
		$sig  = hash_hmac( 'sha256', $body, self::SECRET );

		$status_code = $this->create_gateway()->process_webhook_request( $body, $sig );

		$this->assertSame( 400, $status_code );
	}

	// --- Order lookup. ---------------------------------------------------------

	public function test_unknown_payment_id_is_rejected_cleanly_as_not_found_with_no_fatal() {
		// No order created for this payment_id at all.
		list( $body, $sig ) = $this->build_signed_event(
			array( 'event' => 'order.paid', 'payment_id' => 'pay_does_not_exist_on_this_site', 'status' => 'paid' )
		);

		$status_code = $this->create_gateway()->process_webhook_request( $body, $sig );

		$this->assertSame( 404, $status_code, 'An unknown payment_id (already-authenticated) should fail cleanly as not-found, never fatal.' );
	}

	// --- event_id dedupe. --------------------------------------------------------

	public function test_a_repeated_event_id_is_skipped_and_not_double_processed() {
		$order = $this->create_order_for_payment_id( 'pay_receiver_dedupe', 'pending' );
		list( $body, $sig ) = $this->build_signed_event(
			array( 'event' => 'order.expired', 'payment_id' => 'pay_receiver_dedupe', 'status' => 'expired' )
		);
		$gateway = $this->create_gateway();

		$first  = $gateway->process_webhook_request( $body, $sig );
		$after_first = wc_get_order( $order->get_id() );
		$notes_after_first = count( wc_get_order_notes( array( 'order_id' => $order->get_id() ) ) );

		$this->assertSame( 200, $first );
		$this->assertSame( 'cancelled', $after_first->get_status() );

		// Exact same delivery, redelivered (retry, or a lost ack) - same raw
		// body, same signature, same event_id.
		$second = $gateway->process_webhook_request( $body, $sig );
		$after_second = wc_get_order( $order->get_id() );
		$notes_after_second = count( wc_get_order_notes( array( 'order_id' => $order->get_id() ) ) );

		$this->assertSame( 200, $second, 'A retried delivery must still be acknowledged with 200 - the engine already considers it delivered.' );
		$this->assertSame( 'cancelled', $after_second->get_status(), 'Status must not be touched a second time.' );
		$this->assertSame(
			$notes_after_first,
			$notes_after_second,
			'The status-transition note must not be written a second time for a deduped redelivery.'
		);
	}

	public function test_two_different_events_for_the_same_order_are_both_applied() {
		// The dedupe must be keyed by event_id, not merely "have we ever
		// touched this order before" - a real order legitimately receives
		// several distinct events over its lifetime.
		$order = $this->create_order_for_payment_id( 'pay_receiver_two_events', 'pending' );
		$gateway = $this->create_gateway();

		list( $body1, $sig1 ) = $this->build_signed_event(
			array( 'event' => 'order.confirming', 'payment_id' => 'pay_receiver_two_events', 'status' => 'confirming' )
		);
		$this->assertSame( 200, $gateway->process_webhook_request( $body1, $sig1 ) );
		$this->assertSame( 'on-hold', wc_get_order( $order->get_id() )->get_status() );

		list( $body2, $sig2 ) = $this->build_signed_event(
			array( 'event' => 'order.paid', 'payment_id' => 'pay_receiver_two_events', 'status' => 'paid' )
		);
		$this->assertSame( 200, $gateway->process_webhook_request( $body2, $sig2 ) );
		$this->assertTrue( wc_get_order( $order->get_id() )->has_status( array( 'processing', 'completed' ) ) );
	}
}
