<?php
/**
 * The plugin's W3C Trace Context support (structured_logging.md 2.4): one
 * trace id per PHP request, sent as `traceparent` on every call to
 * Monokulo, and taken from an incoming webhook's `traceparent` so the
 * webhook's log lines join the engine's delivery attempt.
 *
 * @package Monokulo
 */

/**
 * Asserts the trace helpers on `WC_Gateway_Monokulo`.
 */
class TraceContextTest extends WP_UnitTestCase {

	public function test_a_traceparent_names_this_requests_trace_and_a_new_span_each_time() {
		$first  = WC_Gateway_Monokulo::traceparent();
		$second = WC_Gateway_Monokulo::traceparent();
		$this->assertMatchesRegularExpression( '/^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/', $first );
		$this->assertSame( substr( $first, 3, 32 ), substr( $second, 3, 32 ), 'One trace for the whole PHP request.' );
		$this->assertNotSame( substr( $first, 36, 16 ), substr( $second, 36, 16 ), 'Each call is its own span.' );
		$this->assertSame( WC_Gateway_Monokulo::trace_id(), substr( $first, 3, 32 ) );
	}

	public function test_an_incoming_traceparent_is_adopted_only_when_well_formed() {
		$this->assertTrue( WC_Gateway_Monokulo::adopt_traceparent( '00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' ) );
		$this->assertSame( '4bf92f3577b34da6a3ce929d0e0e4736', WC_Gateway_Monokulo::trace_id() );

		foreach ( array(
			'',
			'garbage',
			'00-00000000000000000000000000000000-00f067aa0ba902b7-01',
			'00-5bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01',
			'00-5BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01',
			'01-5bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',
			"00-5bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\nX-Injected: 1",
		) as $bad ) {
			$this->assertFalse( WC_Gateway_Monokulo::adopt_traceparent( $bad ), $bad );
		}
		$this->assertSame( '4bf92f3577b34da6a3ce929d0e0e4736', WC_Gateway_Monokulo::trace_id(), 'A bad header changes nothing.' );
	}

	/**
	 * A connected gateway, with "Send errors to Monokulo" set as given.
	 *
	 * @param string $forward 'yes' or 'no'.
	 * @return WC_Gateway_Monokulo
	 */
	private function gateway_forwarding( $forward ) {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'endpoint', 'http://monokulo.test' );
		$seed->update_option( 'public_key', 'pk_test_abc123' );
		$seed->update_option( 'secret_token', 'sk_test_secret' );
		$seed->update_option( 'connection_version', WC_Gateway_Monokulo::CONNECTION_VERSION );
		$seed->update_option( 'forward_errors', $forward );
		return new WC_Gateway_Monokulo();
	}

	public function test_warnings_are_sent_to_monokulo_only_when_the_option_is_on() {
		$captured = array();
		add_filter(
			'pre_http_request',
			function ( $preempt, $args, $url ) use ( &$captured ) {
				$captured[] = array(
					'url'  => $url,
					'args' => $args,
				);
				return array(
					'body'     => '',
					'response' => array(
						'code'    => 204,
						'message' => 'No Content',
					),
				);
			},
			10,
			3
		);

		// Off (the default): nothing is queued.
		$this->gateway_forwarding( 'no' )->process_webhook_request( '{}', 'bad-signature' );
		$this->assertNull( WC_Gateway_Monokulo::flush_forwarded( 'http://monokulo.test/pay/pk_test_abc123/logs', 'sk_test_secret' ) );

		// On: a rejected webhook's warning is queued, then sent in one request.
		$this->gateway_forwarding( 'yes' )->process_webhook_request( '{}', 'bad-signature' );
		$args = WC_Gateway_Monokulo::flush_forwarded( 'http://monokulo.test/pay/pk_test_abc123/logs', 'sk_test_secret' );
		$this->assertNotNull( $args );
		$this->assertSame( 'Bearer sk_test_secret', $args['headers']['Authorization'] );
		$this->assertFalse( $args['blocking'], 'The page never waits for it.' );
		$body = json_decode( $args['body'], true );
		$this->assertSame( 'warning', $body['entries'][0]['level'] );
		$this->assertStringContainsString( 'Webhook rejected', $body['entries'][0]['message'] );
		$this->assertSame( WC_Gateway_Monokulo::trace_id(), $body['entries'][0]['trace_id'] );
		$this->assertSame( 'http://monokulo.test/pay/pk_test_abc123/logs', end( $captured )['url'] );

		$this->assertNull( WC_Gateway_Monokulo::flush_forwarded( 'http://monokulo.test/pay/pk_test_abc123/logs', 'sk_test_secret' ), 'Sent once.' );
		remove_all_filters( 'pre_http_request' );
	}
}
