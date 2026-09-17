<?php
/**
 * WBS 1.5.4's table-driven status-mapping acceptance tests: "table-driven
 * PHPUnit tests for the status mapping (each engine status → expected WC
 * status)."
 *
 * The full event catalog exercised here is checked directly against every
 * `enqueue_webhook_event()` call site in `src/scanner.rs` (grepped for
 * `"order\.`, not assumed from this step's own brief alone) - nine event
 * types total: one `order.<status>` event per `src/status.rs::OrderStatus`
 * variant (seven, confirmed exhaustive by reading that enum directly), plus
 * `order.double_spend_detected`/`order.double_spend_reversed` from
 * `void_and_notify()`/`unvoid_as_false_positive()`. This file covers all
 * nine - see `WC_Gateway_Monokulo::apply_order_status_event()`'s own doc
 * comment for the full reasoning behind each row of the table asserted
 * below, and `apply_webhook_event()`'s for why the two double-spend events
 * are deliberately *not* part of the same status table (they never set a WC
 * status themselves - see that method's doc comment for why).
 *
 * @package Monokulo
 */
class WebhookStatusMappingTest extends WP_UnitTestCase {

	const SECRET = 'whsec_status_mapping_test_secret';

	private function create_gateway() {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'webhook_signing_secret', self::SECRET );
		return new WC_Gateway_Monokulo();
	}

	private function create_order_for_payment_id( $payment_id, $status = 'pending' ) {
		$order = wc_create_order();
		$order->set_status( $status );
		$order->update_meta_data( WC_Gateway_Monokulo::META_PAYMENT_ID, $payment_id );
		$order->save();
		return $order;
	}

	private function send_event( WC_Gateway_Monokulo $gateway, array $fields ) {
		$body = wp_json_encode(
			array_merge(
				array(
					'event_id'   => 'evt_' . wp_generate_password( 12, false ),
					'created_at' => time(),
				),
				$fields
			)
		);
		$sig = hash_hmac( 'sha256', $body, self::SECRET );
		return $gateway->process_webhook_request( $body, $sig );
	}

	/**
	 * The core table: every `order.<status>` event this engine sends, and
	 * the WooCommerce status it must produce. `paid`/`overpaid` assert
	 * against a *set* of acceptable statuses (`processing` or `completed`)
	 * rather than one fixed string - see `apply_order_status_event()`'s own
	 * doc comment on `payment_complete()` for why that choice (virtual vs.
	 * physical goods) is WooCommerce's own to make, not this plugin's, and
	 * asserting one specific value here would be asserting something this
	 * step's own correct behavior does not guarantee for every order.
	 *
	 * @return array<string, array{0: string, 1: string, 2: string[]}>
	 *         [event type, payment_id, acceptable resulting WC statuses]
	 */
	public function status_mapping_provider() {
		return array(
			'order.pending -> pending'         => array( 'order.pending', 'pending', array( 'pending' ) ),
			'order.unconfirmed -> on-hold'     => array( 'order.unconfirmed', 'unconfirmed', array( 'on-hold' ) ),
			'order.confirming -> on-hold'      => array( 'order.confirming', 'confirming', array( 'on-hold' ) ),
			'order.partial -> on-hold'         => array( 'order.partial', 'partial', array( 'on-hold' ) ),
			'order.paid -> processing/completed'    => array( 'order.paid', 'paid', array( 'processing', 'completed' ) ),
			'order.overpaid -> processing/completed' => array( 'order.overpaid', 'overpaid', array( 'processing', 'completed' ) ),
			'order.expired -> cancelled'       => array( 'order.expired', 'expired', array( 'cancelled' ) ),
		);
	}

	/**
	 * @dataProvider status_mapping_provider
	 */
	public function test_status_mapping( $event_type, $engine_status, array $acceptable_wc_statuses ) {
		$payment_id = 'pay_mapping_' . str_replace( '.', '_', $event_type );
		$order      = $this->create_order_for_payment_id( $payment_id );
		$gateway    = $this->create_gateway();

		$status_code = $this->send_event(
			$gateway,
			array(
				'event'      => $event_type,
				'payment_id' => $payment_id,
				'status'     => $engine_status,
			)
		);

		$this->assertSame( 200, $status_code, "$event_type should be accepted and processed." );

		$resulting_status = wc_get_order( $order->get_id() )->get_status();
		$this->assertContains(
			$resulting_status,
			$acceptable_wc_statuses,
			"$event_type produced WC status \"$resulting_status\", expected one of: " . implode( ', ', $acceptable_wc_statuses )
		);
	}

	public function test_order_pending_after_a_double_spend_void_wipes_out_the_only_payment_is_a_real_reachable_regression() {
		// A order.pending event arriving for an order that was already past
		// "pending" is not hypothetical - see apply_order_status_event()'s
		// own doc comment: a double-spend void removing an order's only
		// payment makes derive_status() in src/status.rs genuinely return
		// to Pending (total == 0). The mapping must apply this regression
		// faithfully, not resist it.
		$order   = $this->create_order_for_payment_id( 'pay_regression_to_pending', 'on-hold' );
		$gateway = $this->create_gateway();

		$status_code = $this->send_event(
			$gateway,
			array( 'event' => 'order.pending', 'payment_id' => 'pay_regression_to_pending', 'status' => 'pending' )
		);

		$this->assertSame( 200, $status_code );
		$this->assertSame( 'pending', wc_get_order( $order->get_id() )->get_status() );
	}

	public function test_overpaid_adds_an_explicit_manual_refund_note_beyond_the_payment_complete_note() {
		$order   = $this->create_order_for_payment_id( 'pay_overpaid_note' );
		$gateway = $this->create_gateway();

		$this->send_event(
			$gateway,
			array( 'event' => 'order.overpaid', 'payment_id' => 'pay_overpaid_note', 'status' => 'overpaid' )
		);

		$notes = wc_get_order_notes( array( 'order_id' => $order->get_id() ) );
		$note_contents = implode( ' | ', wp_list_pluck( $notes, 'content' ) );
		$this->assertStringContainsString(
			'more Monero than the amount due',
			$note_contents,
			'Overpayment must be flagged explicitly, since payment_complete() alone has no way to say this.'
		);
	}

	public function test_expired_note_explains_manual_handling_is_required_for_any_partial_funds() {
		$order   = $this->create_order_for_payment_id( 'pay_expired_note' );
		$gateway = $this->create_gateway();

		$this->send_event(
			$gateway,
			array( 'event' => 'order.expired', 'payment_id' => 'pay_expired_note', 'status' => 'expired' )
		);

		$notes = wc_get_order_notes( array( 'order_id' => $order->get_id() ) );
		$note_contents = implode( ' | ', wp_list_pluck( $notes, 'content' ) );
		$this->assertStringContainsString( 'no automatic refund', $note_contents );
	}

	// --- The two double-spend events: no status table row, note-only. --------

	public function test_double_spend_detected_adds_a_prominent_note_without_changing_status_by_itself() {
		// An order that a paired order.<status> event has *not* also moved -
		// simulating the case where other payments still cover the order,
		// so recompute_and_notify_in_tx() found no transition and only
		// void_and_notify()'s own order.double_spend_detected event fired.
		$order   = $this->create_order_for_payment_id( 'pay_double_spend_detected', 'processing' );
		$gateway = $this->create_gateway();

		$status_code = $this->send_event(
			$gateway,
			array( 'event' => 'order.double_spend_detected', 'payment_id' => 'pay_double_spend_detected' )
		);

		$this->assertSame( 200, $status_code );
		$reloaded = wc_get_order( $order->get_id() );
		$this->assertSame(
			'processing',
			$reloaded->get_status(),
			'double_spend_detected must never itself change status - any real consequence is announced via its own paired order.<status> event.'
		);

		$notes = wc_get_order_notes( array( 'order_id' => $order->get_id() ) );
		$note_contents = implode( ' | ', wp_list_pluck( $notes, 'content' ) );
		$this->assertStringContainsString(
			'FRAUD ALERT',
			$note_contents,
			'The double-spend note must be unmissable, per this step\'s own brief: the merchant needs to notice, not have it silently swallowed.'
		);
	}

	public function test_double_spend_reversed_adds_a_note_with_the_txid_without_changing_status_by_itself() {
		$order   = $this->create_order_for_payment_id( 'pay_double_spend_reversed', 'on-hold' );
		$gateway = $this->create_gateway();

		$status_code = $this->send_event(
			$gateway,
			array(
				'event'      => 'order.double_spend_reversed',
				'payment_id' => 'pay_double_spend_reversed',
				'txid'       => 'abc123deadbeef',
			)
		);

		$this->assertSame( 200, $status_code );
		$reloaded = wc_get_order( $order->get_id() );
		$this->assertSame(
			'on-hold',
			$reloaded->get_status(),
			'double_spend_reversed must not itself set a status - the real recomputed status (if it changed) is ' .
			'delivered via its own paired order.<status> event, per unvoid_as_false_positive() in src/scanner.rs.'
		);

		$notes = wc_get_order_notes( array( 'order_id' => $order->get_id() ) );
		$note_contents = implode( ' | ', wp_list_pluck( $notes, 'content' ) );
		$this->assertStringContainsString( 'false positive', $note_contents );
		$this->assertStringContainsString( 'abc123deadbeef', $note_contents, 'The txid the reversal was about should appear in the note.' );
	}
}
