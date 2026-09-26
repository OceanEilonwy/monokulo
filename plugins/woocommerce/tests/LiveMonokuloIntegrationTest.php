<?php
/**
 * The *live* half of the plugin's order-creation tests: `process_payment()`
 * against a real, running Monokulo (and the engine behind it) - not
 * `pre_http_request`-mocked like `ProcessPaymentTest.php`.
 *
 * **Excluded from the default run** (`@group live-monokulo`, excluded in
 * `phpunit.xml.dist`) for the same reason the Rust stagenet tests are
 * `#[ignore]`d by default: it needs real, running services this suite's
 * default hermetic run must never silently depend on. Run it explicitly,
 * once the services below are up: `vendor/bin/phpunit --group live-monokulo`.
 *
 * The full WooCommerce checkout (connect, keyed order creation, checkout
 * page, paid webhook) also runs by default, without any of this setup, in
 * the Rust `mock-woocommerce` crate's
 * `a_full_woocommerce_checkout_is_created_with_the_key_opened_and_paid` test.
 *
 * ## What this test needs
 *
 * 1. A running engine and Monokulo, e.g. `scripts/dev-run.sh` from the repo
 *    root, with Monokulo's `public_url` set to the address the tests
 *    container reaches it at (below).
 * 2. A store connected on that Monokulo (the dashboard's "Advanced setup"
 *    works), and its public and secret key. The secret key is what the
 *    connect flow would hand this plugin; for this test copy it from
 *    Monokulo's database (`store_connections.tenant_secret_token_encrypted`,
 *    decrypted with `MONOKULO_ENCRYPTION_KEY`) or connect a real WordPress
 *    site once and read the plugin's saved `secret_token` setting.
 * 3. A way for wp-env's `tests-cli` container to reach Monokulo. Monokulo
 *    listens on `127.0.0.1:8081` only (`crates/monokulo/src/main.rs`), and a
 *    container's `127.0.0.1` is itself, not the host. Either run Monokulo in
 *    a container on wp-env's Docker network with a forwarder such as
 *    `socat TCP-LISTEN:8090,fork,bind=0.0.0.0 TCP:127.0.0.1:8081` beside it
 *    and use `http://<that container's name>:8090`, or forward a host port
 *    to it and use `http://host.docker.internal:<port>` (some host
 *    firewalls, `ufw` in particular, block container-to-host connections).
 * 4. `tests/live-monokulo.local.json` next to this file (gitignored):
 *
 *    ```json
 *    { "endpoint": "http://monokulo-test:8090", "public_key": "pk_...", "secret_token": "sk_..." }
 *    ```
 *
 * The store must accept XMR (always priceable) - this test orders in XMR so
 * no exchange-rate provider is needed.
 *
 * @package Monokulo
 */

/**
 * @group live-monokulo
 */
class LiveMonokuloIntegrationTest extends WP_UnitTestCase {

	/**
	 * @var array{endpoint: string, public_key: string, secret_token: string}
	 */
	private $live_config;

	private function live_config_path() {
		return __DIR__ . '/live-monokulo.local.json';
	}

	public function set_up(): void {
		parent::set_up();

		if ( ! file_exists( $this->live_config_path() ) ) {
			$this->markTestSkipped(
				'No tests/live-monokulo.local.json found - this test needs a real, running Monokulo ' .
				'reachable from inside wp-env\'s own Docker network. See this file\'s own class doc ' .
				'comment for how to set one up.'
			);
		}

		$decoded = json_decode( (string) file_get_contents( $this->live_config_path() ), true );
		if ( ! is_array( $decoded ) || empty( $decoded['endpoint'] ) || empty( $decoded['public_key'] ) || empty( $decoded['secret_token'] ) ) {
			$this->fail( 'tests/live-monokulo.local.json exists but is missing endpoint, public_key and/or secret_token.' );
		}
		$this->live_config = $decoded;

		// WooCommerce has no XMR out of the box; a Monero store adds it.
		add_filter(
			'woocommerce_currencies',
			function ( $currencies ) {
				$currencies['XMR'] = 'Monero';
				return $currencies;
			}
		);
	}

	private function create_real_order() {
		$order = wc_create_order();
		$this->assertNotWPError( $order );

		$order->set_currency( 'XMR' );
		$order->set_total( '0.05' );
		$order->save();

		return $order;
	}

	/**
	 * `process_payment()` against a real `WC_Order` really creates a
	 * Monokulo order with the store's secret key and hands WooCommerce the
	 * real `{endpoint}/pay/{pk}/orders/{order_id}` checkout page. Then,
	 * independently of the gateway's own return value, this test asks
	 * Monokulo itself (its public status route) whether that order exists,
	 * and fetches the checkout page, so a gateway that merely built a
	 * plausible URL couldn't pass.
	 */
	public function test_process_payment_creates_a_real_monokulo_order_and_redirects_to_its_checkout() {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'enabled', 'yes' );
		$seed->update_option( 'endpoint', $this->live_config['endpoint'] );
		$seed->update_option( 'public_key', $this->live_config['public_key'] );
		$seed->update_option( 'secret_token', $this->live_config['secret_token'] );
		$seed->update_option( 'connection_version', WC_Gateway_Monokulo::CONNECTION_VERSION );
		$gateway = new WC_Gateway_Monokulo(); // Re-read back from wp_options, like a real request would.

		$order  = $this->create_real_order();
		$result = $gateway->process_payment( $order->get_id() );

		$this->assertSame( 'success', $result['result'] );
		$checkout_prefix = sprintf(
			'%s/pay/%s/orders/',
			rtrim( $this->live_config['endpoint'], '/' ),
			rawurlencode( $this->live_config['public_key'] )
		);
		$this->assertStringStartsWith( $checkout_prefix, $result['redirect'] );
		$order_id = substr( $result['redirect'], strlen( $checkout_prefix ) );
		$this->assertNotEmpty( $order_id );
		$this->assertSame( $order_id, wc_get_order( $order->get_id() )->get_meta( '_monokulo_order_id' ) );

		// --- The independent check: ask Monokulo itself. ---
		$status_response = wp_remote_get( $result['redirect'] . '/status', array( 'timeout' => 15 ) );
		$this->assertNotWPError( $status_response );
		$this->assertSame( 200, wp_remote_retrieve_response_code( $status_response ) );
		$status = json_decode( wp_remote_retrieve_body( $status_response ), true );
		$this->assertSame( 'pending', $status['status'], 'A brand new order should still be pending.' );
		$this->assertSame( 0, $status['confirmations'] );

		$page = wp_remote_get( $result['redirect'], array( 'timeout' => 15 ) );
		$this->assertSame( 200, wp_remote_retrieve_response_code( $page ) );
		$this->assertStringContainsString( '<html', wp_remote_retrieve_body( $page ) );
	}
}
