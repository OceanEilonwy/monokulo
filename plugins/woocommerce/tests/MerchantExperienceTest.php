<?php
/**
 * What the merchant sees on the gateway's settings screen after connecting,
 * and how a shop copes with Monokulo answers it cannot use: a response that
 * is not Monokulo's (a proxy's error page), a store Monokulo no longer
 * knows, and webhook events from a newer Monokulo than this plugin.
 *
 * @package Monokulo
 */

/**
 * Settings-screen notices and resilience cases.
 */
class MerchantExperienceTest extends WP_UnitTestCase {

	/**
	 * Log entries captured from `wc_get_logger()` while a test runs.
	 *
	 * @var array<int, array{0: string, 1: string}>
	 */
	private $logged = array();

	public function set_up(): void {
		parent::set_up();
		$this->logged = array();
		$logged       = &$this->logged;
		// `wc_get_logger()` uses an object from this filter as the logger itself.
		add_filter(
			'woocommerce_logging_class',
			function () use ( &$logged ) {
				return new class( $logged ) extends WC_Logger {
					private $entries;
					public function __construct( &$entries ) {
						$this->entries = &$entries;
					}
					public function log( $level, $message, $context = array() ) {
						$this->entries[] = array( $level, $message );
					}
				};
			}
		);
	}

	public function tear_down(): void {
		remove_all_filters( 'pre_http_request' );
		remove_all_filters( 'woocommerce_logging_class' );
		$_GET = array();
		parent::tear_down();
	}

	private function connected_gateway() {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'enabled', 'yes' );
		$seed->update_option( 'endpoint', 'https://monokulo.test' );
		$seed->update_option( 'public_key', 'pk_shop_connected' );
		$seed->update_option( 'secret_token', 'sk_shop_secret' );
		$seed->update_option( 'connection_version', WC_Gateway_Monokulo::CONNECTION_VERSION );
		$seed->update_option( 'webhook_signing_secret', 'whsec_experience' );
		return new WC_Gateway_Monokulo();
	}

	private function order_awaiting_monokulo( $order_id ) {
		$order = wc_create_order();
		$order->set_status( 'pending' );
		$order->update_meta_data( WC_Gateway_Monokulo::META_ORDER_ID, $order_id );
		$order->save();
		return $order;
	}

	private function notice_for( array $query ) {
		$_GET = array_merge( array( 'page' => 'wc-settings', 'section' => 'monokulo' ), $query );
		ob_start();
		$this->connected_gateway()->maybe_render_connect_notice();
		return ob_get_clean();
	}

	/**
	 * Back from Monokulo's connect page, the merchant is told how it went:
	 * connected, Monokulo not ready yet (its operator has not set its public
	 * address), or a failure to retry. Nothing shows on other admin screens.
	 */
	public function test_the_merchant_is_told_how_connecting_went() {
		$this->assertStringContainsString( 'Your Monero wallet is connected', $this->notice_for( array( 'monokulo_connected' => '1' ) ) );
		$this->assertStringContainsString( 'its operator has not set its public address', $this->notice_for( array( 'monokulo_connect_error' => 'unavailable' ) ) );
		$this->assertStringContainsString( 'Could not connect your Monero wallet', $this->notice_for( array( 'monokulo_connect_error' => 'finish_failed' ) ) );
		$this->assertSame( '', $this->notice_for( array( 'section' => 'bacs', 'monokulo_connected' => '1' ) ) );
	}

	/**
	 * A connected store's settings show which Monokulo store it is connected
	 * as, and offer to reconnect.
	 */
	public function test_a_connected_store_shows_which_store_it_is_connected_as() {
		$html = $this->connected_gateway()->generate_monokulo_connect_html( 'monokulo_connect', array() );
		$this->assertStringContainsString( 'Connected as', $html );
		$this->assertStringContainsString( '<code>pk_shop_connected</code>', $html );
	}

	/**
	 * Something in front of Monokulo (a CDN or proxy) answers the order
	 * request with its own page and a 200: the customer is told the payment
	 * could not start, the order is not linked to anything, and the merchant's
	 * log has the body.
	 */
	public function test_a_response_that_is_not_monokulos_fails_the_payment_cleanly() {
		$order = wc_create_order();
		$order->set_currency( 'USD' );
		$order->set_total( '12.50' );
		$order->save();
		add_filter(
			'pre_http_request',
			function () {
				return array(
					'headers'  => array(),
					'body'     => '<html><body>Checking your browser before accessing monokulo.test</body></html>',
					'response' => array( 'code' => 200, 'message' => 'OK' ),
					'cookies'  => array(),
				);
			}
		);
		try {
			$this->connected_gateway()->process_payment( $order->get_id() );
			$this->fail( 'An unusable response must not start a payment.' );
		} catch ( Exception $e ) {
			$this->assertStringContainsString( 'unexpected response', $e->getMessage() );
		}
		$this->assertSame( '', wc_get_order( $order->get_id() )->get_meta( WC_Gateway_Monokulo::META_ORDER_ID ) );
		$this->assertTrue( $this->logged_contains( 'Checking your browser' ) );
	}

	/**
	 * Monokulo no longer knows this store (it was reset or the store was
	 * removed there): the customer gets the generic message and the log tells
	 * the merchant to reconnect.
	 */
	public function test_a_store_monokulo_no_longer_knows_tells_the_merchant_to_reconnect() {
		$order = wc_create_order();
		$order->set_currency( 'USD' );
		$order->set_total( '12.50' );
		$order->save();
		add_filter(
			'pre_http_request',
			function () {
				return array(
					'headers'  => array(),
					'body'     => wp_json_encode( array( 'error' => 'not found' ) ),
					'response' => array( 'code' => 404, 'message' => 'Not Found' ),
					'cookies'  => array(),
				);
			}
		);
		try {
			$this->connected_gateway()->process_payment( $order->get_id() );
			$this->fail( 'A 404 must not start a payment.' );
		} catch ( Exception $e ) {
			$this->assertStringContainsString( 'could not start this payment', $e->getMessage() );
		}
		$this->assertTrue( $this->logged_contains( 'reconnect the plugin' ) );
	}

	/**
	 * A newer Monokulo sends event types this plugin does not know (a new
	 * order status, or a new kind of event): each is acknowledged (so
	 * Monokulo does not retry it forever), the order is left alone, and the
	 * merchant's log says so.
	 */
	public function test_events_from_a_newer_monokulo_are_acknowledged_and_change_nothing() {
		$gateway = $this->connected_gateway();
		$order   = $this->order_awaiting_monokulo( 'order_from_the_future' );
		foreach ( array(
			array( 'event' => 'order.refunded', 'status' => 'refunded' ),
			array( 'event' => 'payment.relabelled', 'status' => 'pending' ),
		) as $fields ) {
			$body = wp_json_encode(
				array_merge(
					array( 'event_id' => 'evt_' . wp_generate_password( 12, false ), 'created_at' => time(), 'order_id' => 'order_from_the_future' ),
					$fields
				)
			);
			$this->assertSame( 200, $gateway->process_webhook_request( $body, monokulo_test_signature( $body, 'whsec_experience' ) ), $fields['event'] );
			$this->assertTrue( wc_get_order( $order->get_id() )->has_status( 'pending' ), $fields['event'] );
		}
		$this->assertTrue( $this->logged_contains( 'unrecognized event type "order.refunded"' ) );
		$this->assertTrue( $this->logged_contains( 'unrecognized event type "payment.relabelled"' ) );
	}

	private function logged_contains( $needle ) {
		foreach ( $this->logged as $entry ) {
			if ( false !== strpos( $entry[1], $needle ) ) {
				return true;
			}
		}
		return false;
	}
}
