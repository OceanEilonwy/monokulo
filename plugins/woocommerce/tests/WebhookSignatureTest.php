<?php
/**
 * The PHP verifier against the Rust signer's fixed known vector, so a bug
 * in either language's signing shows up against the other's real output,
 * not only against itself.
 *
 * The secret, payload, timestamp and header below are copied verbatim from
 * `shared/src/webhook_sign.rs`'s `KNOWN_VECTOR_*` test constants, and the
 * tag was cross-checked outside both languages: `python3 -c "import
 * hmac,hashlib; print(hmac.new(b'known_vector_secret_for_php_crosscheck',
 * b'1700000000.' + payload, hashlib.sha256).hexdigest())"` gives the same
 * `36a7d36d...` - a three-way agreement (Rust, Python, PHP).
 *
 * @package Monokulo
 */
class WebhookSignatureTest extends WP_UnitTestCase {

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s `KNOWN_VECTOR_SECRET`.
	 */
	const KNOWN_VECTOR_SECRET = 'known_vector_secret_for_php_crosscheck';

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s `KNOWN_VECTOR_PAYLOAD`,
	 * with no trailing newline.
	 */
	const KNOWN_VECTOR_PAYLOAD = '{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}';

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s `KNOWN_VECTOR_TIMESTAMP`.
	 */
	const KNOWN_VECTOR_TIMESTAMP = 1700000000;

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s
	 * `KNOWN_VECTOR_SIGNATURE_HEADER`.
	 */
	const KNOWN_VECTOR_SIGNATURE_HEADER = 't=1700000000,v1=36a7d36d510620adf9ae9e3e42891dcc796ae88c6ac19818bab778998d2a35e6';

	/**
	 * A configured gateway whose `webhook_signing_secret` is `$secret`,
	 * written with `update_option()` on one instance and read back by a
	 * second, so the real persisted-settings path is exercised.
	 *
	 * @param string $secret The `webhook_signing_secret` to configure.
	 * @return WC_Gateway_Monokulo
	 */
	private function gateway_with_secret( $secret ) {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'webhook_signing_secret', $secret );
		return new WC_Gateway_Monokulo();
	}

	/**
	 * Calls the private `verify_webhook_signature()` via reflection, at the
	 * known vector's time unless told otherwise.
	 *
	 * @param WC_Gateway_Monokulo $gateway
	 * @param string               $raw_body
	 * @param string               $signature
	 * @param int                  $now
	 * @return bool
	 */
	private function verify( WC_Gateway_Monokulo $gateway, $raw_body, $signature, $now = self::KNOWN_VECTOR_TIMESTAMP ) {
		$method = new ReflectionMethod( WC_Gateway_Monokulo::class, 'verify_webhook_signature' );
		$method->setAccessible( true );
		return $method->invoke( $gateway, $raw_body, $signature, $now );
	}

	/**
	 * A PHP verifier that computed a consistently wrong tag would pass a
	 * self-referential test while failing every real webhook from the
	 * engine; only the Rust signer's own output catches that.
	 */
	public function test_php_hmac_matches_the_known_vector_from_shared_webhook_sign_rs() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );

		$this->assertTrue(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER ),
			'The PHP verifier must accept the exact header the Rust signer produces for this input.'
		);

		// The raw computation, restated for a reader to check by eye.
		$this->assertSame(
			self::KNOWN_VECTOR_SIGNATURE_HEADER,
			't=' . self::KNOWN_VECTOR_TIMESTAMP . ',v1=' . hash_hmac(
				'sha256',
				self::KNOWN_VECTOR_TIMESTAMP . '.' . self::KNOWN_VECTOR_PAYLOAD,
				self::KNOWN_VECTOR_SECRET
			)
		);
	}

	public function test_verification_rejects_a_tampered_payload() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );
		$this->assertFalse(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD . 'x', self::KNOWN_VECTOR_SIGNATURE_HEADER ),
			'A signature computed over a different payload must not verify.'
		);
	}

	public function test_verification_rejects_a_moved_timestamp() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );
		$moved   = str_replace( 't=1700000000', 't=1700000001', self::KNOWN_VECTOR_SIGNATURE_HEADER );
		$this->assertFalse(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, $moved ),
			'The tag signs its own time; presenting it with another must fail.'
		);
	}

	/**
	 * A captured delivery replayed later than the tolerance is refused, as
	 * is one claiming a time too far ahead.
	 */
	public function test_verification_accepts_only_within_the_tolerance_of_now() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );
		$t       = self::KNOWN_VECTOR_TIMESTAMP;
		$window  = WC_Gateway_Monokulo::SIGNATURE_TOLERANCE_SECS;
		$this->assertTrue( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER, $t + $window ) );
		$this->assertTrue( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER, $t - $window ) );
		$this->assertFalse( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER, $t + $window + 1 ) );
		$this->assertFalse( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER, $t - $window - 1 ) );
	}

	public function test_verification_rejects_the_wrong_secret() {
		$gateway = $this->gateway_with_secret( 'a-different-secret-entirely' );
		$this->assertFalse(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER ),
			'A signature valid under one secret must not verify against a gateway configured with a different one.'
		);
	}

	public function test_verification_rejects_a_missing_or_malformed_signature() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );
		$tag     = substr( self::KNOWN_VECTOR_SIGNATURE_HEADER, strlen( 't=1700000000,v1=' ) );
		foreach ( array(
			'',
			$tag,
			'v1=' . $tag . ',t=1700000000',
			't=,v1=' . $tag,
			't=1700000000,v1=' . substr( $tag, 0, 62 ),
			self::KNOWN_VECTOR_SIGNATURE_HEADER . '00',
			self::KNOWN_VECTOR_SIGNATURE_HEADER . "\n",
		) as $bad ) {
			$this->assertFalse( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, $bad ), var_export( $bad, true ) );
		}
	}

	public function test_verification_rejects_when_no_secret_is_configured() {
		// A gateway never connected has no secret: every request is
		// rejected, never trusted by accident.
		$gateway = new WC_Gateway_Monokulo();
		$this->assertFalse( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEADER ) );
	}
}
