<?php
/**
 * WBS 1.5.2's *live* acceptance test: `process_payment()` against a real,
 * running `scanner` engine - not `pre_http_request`-mocked
 * (`ProcessPaymentTest.php` already covers the mocked, hermetic half of the
 * WBS's own stated test bar; this file covers the other half, "one live
 * integration test against a dev engine for the full round trip").
 *
 * **Excluded from the default run** (`@group live-engine`, excluded in
 * `phpunit.xml.dist`) for the same reason `tests/e2e_stagenet.rs` on the Rust
 * side of this repo is `#[ignore]`d by default: this test has a real,
 * unavoidable network dependency (a real engine process) this project's
 * default, hermetic `vendor/bin/phpunit` run must never silently acquire.
 * Run it explicitly, after starting a real engine per the steps below:
 * `vendor/bin/phpunit --group live-engine`.
 *
 * ## How to start a real engine this test can reach
 *
 * wp-env's `tests-cli`/`tests-wordpress` containers (where this whole suite
 * actually runs) are their own Docker network namespace - `127.0.0.1` inside
 * them means the container itself, not the host. Two approaches were tried
 * for real, not assumed, before landing on the one below:
 *
 * 1. **`host.docker.internal`, which wp-env's own generated
 *    `docker-compose.yml` already wires up for every one of its containers**
 *    (`extra_hosts: ['host.docker.internal:host-gateway']` - read directly
 *    from `~/.wp-env/<instance>/docker-compose.yml`, not assumed). This
 *    *does* resolve on this host - `getent hosts host.docker.internal` from
 *    inside `tests-cli` really does return the Docker bridge gateway IP - and
 *    an engine bound to `0.0.0.0` really is reachable from the *host itself*
 *    at that same IP (confirmed with `curl` directly against the bridge
 *    gateway from the host). But a container-to-host connection to that same
 *    IP/port genuinely times out (`curl` exit 7) on this specific machine -
 *    root-caused to `ufw` being active here (`systemctl is-active ufw` ->
 *    `active`) and, per Docker's own long-documented interaction with `ufw`,
 *    default-denying inbound connections that arrive at the host's own
 *    listening socket *via* a Docker bridge interface, even though the
 *    identical destination IP/port is reachable from host-local processes
 *    (which never cross that boundary). No passwordless `sudo` was available
 *    in this environment to add a `ufw`/`DOCKER-USER` allow rule, and doing
 *    so wasn't attempted - a system-wide firewall change is out of scope for
 *    what a single WBS step should be doing to someone's machine. This is a
 *    real, machine-specific obstacle, not a dead end in general: on a host
 *    without `ufw` (or with a rule already permitting it), approach 1 would
 *    likely just work, and is the simpler of the two.
 * 2. **Run the engine as a container on wp-env's own Docker network instead,
 *    reached by container name** - genuinely tested and confirmed working
 *    end to end on this machine, and what this test actually assumes:
 *
 *    ```
 *    # From the repo root:
 *    cargo build --release
 *
 *    # A minimal config - no real stagenet node needed (order creation never
 *    # dials monero_node; see this config's own comments for why a
 *    # deliberately-unreachable placeholder node entry is still required by
 *    # Config::validate()):
 *    mkdir -p /tmp/scanner-test
 *    # ... write /tmp/scanner-test/config.toml - see
 *    # work_notes.md's WBS 1.5.2 entry for the exact contents used here,
 *    # bind = "0.0.0.0:8180".
 *
 *    docker run -d --name scanner-test \
 *      --network wp-env-monokulo-9652ae59_default \
 *      -v "$(pwd)/target/release/scanner:/scanner:ro" \
 *      -v /tmp/scanner-test:/cfg -w /cfg \
 *      archlinux:latest /scanner /cfg/config.toml
 *    ```
 *
 *    (`archlinux:latest`, not e.g. `debian:bookworm-slim`: this binary is
 *    built against whatever glibc the *build host* has - 2.44 on the machine
 *    this was written on, from a rolling-release distro - and a container
 *    with an older glibc fails to even exec it. `archlinux:latest` matched
 *    exactly, confirmed with `ld-linux-x86-64.so.2 --version` before relying
 *    on it, not assumed from the distro name alone. A statically-linked musl
 *    build would sidestep this entirely but wasn't needed here.)
 *
 *    The network name (`wp-env-monokulo-9652ae59_default`) is
 *    *this specific wp-env instance's* project-scoped Docker network name
 *    (`docker network ls` after `wp-env start`, or `docker inspect` any of
 *    its running containers' `NetworkSettings.Networks` - it changes if
 *    wp-env's own instance hash changes) - substitute the real one for
 *    whichever `wp-env` instance is actually running.
 *
 *    Once it's up, its own boot log prints a real, freshly-bootstrapped
 *    tenant: `bootstrapped self-hosted tenant: public_key=pk_...`. Write
 *    `tests/live-engine.local.json` (gitignored - see `.gitignore`'s own
 *    comment on it) next to this file:
 *
 *    ```json
 *    { "endpoint": "http://scanner-test:8180", "public_key": "pk_..." }
 *    ```
 *
 *    `scanner-test` (the container *name*) is what resolves here,
 *    not an IP or `localhost` - ordinary Docker embedded DNS between two
 *    containers on the same user-defined bridge network, which is a
 *    different (and, on this host, actually working) path than a
 *    container reaching back out to a host-bound port.
 *
 * @package Monokulo
 */

/**
 * @group live-engine
 */
class LiveEngineIntegrationTest extends WP_UnitTestCase {

	/**
	 * @var array{endpoint: string, public_key: string}
	 */
	private $live_config;

	private function live_config_path() {
		return __DIR__ . '/live-engine.local.json';
	}

	public function set_up(): void {
		parent::set_up();

		if ( ! file_exists( $this->live_config_path() ) ) {
			$this->markTestSkipped(
				'No tests/live-engine.local.json found - this test needs a real, running scanner ' .
				'engine reachable from inside wp-env\'s own Docker network. See this file\'s own class doc ' .
				'comment for exactly how to start one and generate this file.'
			);
		}

		$decoded = json_decode( (string) file_get_contents( $this->live_config_path() ), true );
		if ( ! is_array( $decoded ) || empty( $decoded['endpoint'] ) || empty( $decoded['public_key'] ) ) {
			$this->fail( 'tests/live-engine.local.json exists but is missing endpoint and/or public_key.' );
		}
		$this->live_config = $decoded;
	}

	private function create_real_order() {
		$order = wc_create_order();
		$this->assertNotWPError( $order );

		$order->set_currency( 'USD' );
		// Matches this suite's own `[exchange_rate.rates] USD = "0.0067"` fixture
		// config (see this file's class doc comment) - any two-decimal-place USD
		// amount would do, this one just keeps the resulting xmr_amount_piconero
		// small and human-checkable in the assertions below.
		$order->set_total( '5.00' );
		$order->save();

		return $order;
	}

	/**
	 * The core WBS 1.5.2 live acceptance assertion: `process_payment()`
	 * against a real `WC_Order` really calls the real engine's real
	 * `POST /api/v1/t/{pk}/orders`, gets back a real `payment_id`, and hands
	 * WooCommerce a real `/pay/v1/{pk}/{payment_id}` redirect - then,
	 * independently of the gateway's own return value, this test asks the
	 * *engine itself* whether that order really exists, so a bug that made
	 * `process_payment()` merely *construct* a plausible-looking redirect
	 * URL without the engine ever having created anything couldn't pass this
	 * test by accident.
	 */
	public function test_process_payment_creates_a_real_engine_order_and_redirects_to_it() {
		$seed = new WC_Gateway_Monokulo();
		$seed->update_option( 'endpoint', $this->live_config['endpoint'] );
		$seed->update_option( 'public_key', $this->live_config['public_key'] );
		$gateway = new WC_Gateway_Monokulo(); // Re-read back from wp_options, like a real request would.

		$order = $this->create_real_order();

		$result = $gateway->process_payment( $order->get_id() );

		$this->assertSame( 'success', $result['result'] );

		$expected_redirect_prefix = sprintf(
			'%s/pay/v1/%s/',
			rtrim( $this->live_config['endpoint'], '/' ),
			rawurlencode( $this->live_config['public_key'] )
		);
		$this->assertStringStartsWith(
			$expected_redirect_prefix,
			$result['redirect'],
			'redirect should point at the real engine\'s own /pay/v1/{pk}/{payment_id} checkout page.'
		);
		$payment_id = substr( $result['redirect'], strlen( $expected_redirect_prefix ) );
		$this->assertNotEmpty( $payment_id );
		$this->assertSame(
			$payment_id,
			$order->get_meta( '_monokulo_payment_id' ),
			'The payment_id in the redirect URL and the one recorded on the order should be the same value.'
		);

		// --- The independent, non-circular check: ask the engine itself. ---
		$status_url = sprintf(
			'%s/api/v1/t/%s/orders/%s',
			rtrim( $this->live_config['endpoint'], '/' ),
			rawurlencode( $this->live_config['public_key'] ),
			rawurlencode( $payment_id )
		);
		$status_response = wp_remote_get( $status_url, array( 'timeout' => 15 ) );
		$this->assertNotWPError(
			$status_response,
			'Could not reach the live engine\'s own order-status endpoint directly - is it still running ' .
			'and reachable from this container? (' . ( is_wp_error( $status_response ) ? $status_response->get_error_message() : '' ) . ')'
		);
		$this->assertSame( 200, wp_remote_retrieve_response_code( $status_response ) );

		$engine_order = json_decode( wp_remote_retrieve_body( $status_response ), true );
		$this->assertIsArray( $engine_order );
		$this->assertSame( $payment_id, $engine_order['payment_id'] );
		$this->assertSame(
			'pending',
			$engine_order['status'],
			'A brand new order the engine just created for this test should still be pending.'
		);
		$this->assertGreaterThan(
			0,
			$engine_order['xmr_amount_piconero'],
			'The engine should have computed a real, positive XMR amount for the $5.00 order.'
		);
		$this->assertSame( 0, $engine_order['confirmations'] );
	}
}
