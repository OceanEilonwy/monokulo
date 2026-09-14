<?php
/**
 * WBS 1.5.4's own explicitly-named acceptance test: "a unit test running the
 * PHP verifier against 0.3's fixed known-vector test case, asserting the
 * identical result."
 *
 * The secret/payload/expected-signature triple below is copied verbatim from
 * `shared/src/webhook_sign.rs`'s own `#[cfg(test)] mod tests`'s
 * `KNOWN_VECTOR_SECRET`/`KNOWN_VECTOR_PAYLOAD`/`KNOWN_VECTOR_SIGNATURE_HEX`
 * constants (read directly, not regenerated or hand-computed) - that file's
 * own comment states the Rust side's half of this contract explicitly:
 * "This exact secret/payload/signature triple is the vector the *real*
 * WooCommerce plugin's PHP implementation (WBS 1.5.4) must reproduce
 * byte-for-byte to prove its HMAC-SHA256 signing matches this Rust
 * implementation." This file is that reproduction.
 *
 * Independently cross-checked outside both languages entirely before this
 * file was written, so a bug shared by *both* implementations (e.g. a typo
 * in the vector itself, copied into both) couldn't hide behind an agreement
 * that proves nothing: `python3 -c "import hmac,hashlib; print(hmac.new(
 * b'known_vector_secret_for_php_crosscheck', b'{\"event\":\"order.paid\",
 * \"order_id\":\"12345\",\"amount_piconero\":\"1000000000000\"}',
 * hashlib.sha256).hexdigest())"` and a standalone `php -r
 * 'echo hash_hmac("sha256", $payload, $secret);'` invocation both produced
 * the identical `436a60c6f66d20b611c7e4a3f78ab13167fb26680a65d8b2e5a114c182de80f1`
 * this test asserts below - a three-way agreement (Rust, Python stdlib, PHP
 * stdlib), not just PHP checking its own work.
 *
 * @package MoneroPayCloud
 */
class WebhookSignatureTest extends WP_UnitTestCase {

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s own `KNOWN_VECTOR_SECRET`.
	 */
	const KNOWN_VECTOR_SECRET = 'known_vector_secret_for_php_crosscheck';

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s own `KNOWN_VECTOR_PAYLOAD`
	 * - a raw byte string with no trailing newline, exactly as that Rust
	 * `br#"..."#` literal specifies.
	 */
	const KNOWN_VECTOR_PAYLOAD = '{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}';

	/**
	 * Verbatim from `shared/src/webhook_sign.rs`'s own
	 * `KNOWN_VECTOR_SIGNATURE_HEX`.
	 */
	const KNOWN_VECTOR_SIGNATURE_HEX = '436a60c6f66d20b611c7e4a3f78ab13167fb26680a65d8b2e5a114c182de80f1';

	/**
	 * Builds a configured gateway whose `webhook_signing_secret` is the given
	 * value - a fresh instance each time, matching `ProcessPaymentTest::
	 * create_configured_gateway()`'s own pattern of writing via
	 * `update_option()` on one instance and reading back via a second, so
	 * this test exercises the real persisted-settings path rather than
	 * poking a private property directly.
	 *
	 * @param string $secret The `webhook_signing_secret` to configure.
	 * @return WC_Gateway_MoneroPay
	 */
	private function gateway_with_secret( $secret ) {
		$seed = new WC_Gateway_MoneroPay();
		$seed->update_option( 'webhook_signing_secret', $secret );
		return new WC_Gateway_MoneroPay();
	}

	/**
	 * Reaches `verify_webhook_signature()` (private) via reflection - the
	 * only way to test this one piece of logic in true isolation from the
	 * rest of `process_webhook_request()`'s envelope/order-lookup machinery,
	 * which this specific test has no interest in exercising: WBS 1.5.4's
	 * own brief calls the known-vector check out as its own explicit,
	 * standalone acceptance test, distinct from the request-handling tests
	 * in `WebhookReceiverTest.php`.
	 *
	 * @param WC_Gateway_MoneroPay $gateway
	 * @param string               $raw_body
	 * @param string               $signature
	 * @return bool
	 */
	private function verify( WC_Gateway_MoneroPay $gateway, $raw_body, $signature ) {
		$method = new ReflectionMethod( WC_Gateway_MoneroPay::class, 'verify_webhook_signature' );
		$method->setAccessible( true );
		return $method->invoke( $gateway, $raw_body, $signature );
	}

	/**
	 * The mandatory known-vector test itself. Every other test in this
	 * plugin's webhook suite depends on this HMAC implementation actually
	 * being correct - a bug here would be invisible any other way, since a
	 * PHP implementation that computes a *consistently wrong* signature
	 * would still pass a self-referential test (sign with PHP, verify with
	 * the same PHP) while failing every real webhook from the engine
	 * silently, in production, with no error anywhere to explain why.
	 */
	public function test_php_hmac_matches_the_known_vector_from_shared_webhook_sign_rs() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );

		$this->assertTrue(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEX ),
			'PHP\'s hash_hmac(\'sha256\', ...) must reproduce the exact known-vector signature the Rust ' .
			'implementation produces for the identical secret/payload - a mismatch here means every real ' .
			'webhook from the engine would fail signature verification in production.'
		);

		// Restated directly (not via the private verify_webhook_signature()
		// wrapper) so this test also stands as documentation of the raw
		// computation any future reader can eyeball independently of this
		// class's own plumbing.
		$this->assertSame(
			self::KNOWN_VECTOR_SIGNATURE_HEX,
			hash_hmac( 'sha256', self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SECRET )
		);
	}

	public function test_verification_rejects_a_tampered_payload() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );
		$this->assertFalse(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD . 'x', self::KNOWN_VECTOR_SIGNATURE_HEX ),
			'A signature computed over a different payload must not verify.'
		);
	}

	public function test_verification_rejects_the_wrong_secret() {
		$gateway = $this->gateway_with_secret( 'a-different-secret-entirely' );
		$this->assertFalse(
			$this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEX ),
			'A signature valid under one secret must not verify against a gateway configured with a different one.'
		);
	}

	public function test_verification_rejects_a_missing_signature() {
		$gateway = $this->gateway_with_secret( self::KNOWN_VECTOR_SECRET );
		$this->assertFalse( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, '' ) );
	}

	public function test_verification_rejects_when_no_secret_is_configured() {
		// A gateway that has never been connected (WBS 1.5.3) - has_
		// credentials()-style "don't offer what you can't honor" reasoning
		// applied to the webhook side: no secret means every request must
		// be rejected, never trusted by accident.
		$gateway = new WC_Gateway_MoneroPay();
		$this->assertFalse( $this->verify( $gateway, self::KNOWN_VECTOR_PAYLOAD, self::KNOWN_VECTOR_SIGNATURE_HEX ) );
	}
}
