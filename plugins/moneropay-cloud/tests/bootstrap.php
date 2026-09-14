<?php
/**
 * PHPUnit bootstrap for the MoneroPay Cloud WooCommerce plugin.
 *
 * This follows the standard WordPress plugin PHPUnit bootstrap shape (the
 * same one WordPress core's own plugin-developer handbook documents, and the
 * one `wp-env`'s own README section "Using included WordPress PHPUnit test
 * files" assumes): load the WP core test scaffolding's helper functions
 * first, register a `muplugins_loaded` callback that manually `require`s the
 * plugin(s) this suite needs (since PHPUnit's test run bypasses WordPress's
 * normal plugin-activation database bookkeeping entirely - there is no
 * `wp_options` row saying "moneropay-cloud is active" for the test run to
 * discover on its own), then hand off to the real WP core bootstrap, which
 * installs a fresh test database and turns control over to PHPUnit.
 *
 * @package MoneroPayCloud
 */

// `wp-env`'s `tests-cli`/`tests-wordpress` containers set `WP_TESTS_DIR` to
// `/wordpress-phpunit` (confirmed by reading the installed `@wordpress/env`
// package's own `lib/runtime/docker/build-docker-compose-config.js` directly,
// not assumed from its README, which documents the variable's existence but
// not this exact path) - that's the WordPress core PHPUnit test suite
// (`includes/functions.php`, `includes/bootstrap.php`, `WP_UnitTestCase`,
// etc.), bundled into the container specifically so a plugin's own test suite
// never has to fetch or manage a copy of it itself. The `/tmp/...` fallback
// below is the same one WordPress core's own plugin-developer handbook
// documents for running outside `wp-env` (e.g. a bare `svn co` of the test
// library) - kept so this bootstrap isn't silently `wp-env`-only.
$_tests_dir = getenv( 'WP_TESTS_DIR' );
if ( ! $_tests_dir ) {
	$_tests_dir = '/tmp/wordpress-tests-lib';
}

if ( ! file_exists( $_tests_dir . '/includes/functions.php' ) ) {
	fwrite(
		STDERR,
		"Could not find {$_tests_dir}/includes/functions.php - is WP_TESTS_DIR set correctly? " .
		"(expected to be running inside wp-env's tests-cli container)\n"
	);
	exit( 1 );
}

// Composer's autoloader - needed for `yoast/phpunit-polyfills`, which the WP
// core test suite's own bootstrap (required below) expects to be able to
// autoload via `Yoast\PHPUnitPolyfills\...` classes. Without this, WP core's
// bootstrap fails with its own explicit "please install yoast/phpunit-
// polyfills" error rather than a confusing unrelated one - confirmed by
// hitting that exact error first, before adding this require, rather than
// assuming it was needed.
require_once dirname( __DIR__ ) . '/vendor/autoload.php';

// Gives access to `tests_add_filter()`, the WP test scaffolding's own thin
// wrapper around `add_filter()` that works even this early, before WordPress
// itself has finished loading (plain `add_filter()` isn't safely callable
// yet at this point in the bootstrap sequence).
require_once $_tests_dir . '/includes/functions.php';

/**
 * Loads WooCommerce, then this plugin, at the `muplugins_loaded` hook -
 * the earliest point in WordPress's own bootstrap sequence a test's manually
 * `require`d plugin can safely run (mirroring how a *real* must-use plugin
 * loads, which is also why this specific hook is the WP core test suite's
 * own documented mechanism for "manually load a plugin for testing").
 *
 * WooCommerce has to load first, in this same callback, for the same reason
 * `moneropay-cloud.php`'s own `plugins_loaded` hook exists in production:
 * `WC_Gateway_MoneroPay extends WC_Payment_Gateway`, a class WooCommerce
 * itself defines, so it must already exist by the time this plugin's files
 * are `require`d. In a normal WordPress request that ordering is WordPress's
 * own job (both plugins are "active", and `moneropay-cloud.php` additionally
 * defers its own class-loading to `plugins_loaded`, a hook WooCommerce is
 * guaranteed to have already fired `plugins_loaded` for by the time it
 * fires). Here there is no such WordPress-managed ordering at all - both
 * plugins are `require`d directly, by this test suite, so this function has
 * to reconstruct that same ordering by hand.
 */
function _moneropay_cloud_manually_load_plugins() {
	$woocommerce_main_file = WP_CONTENT_DIR . '/plugins/woocommerce/woocommerce.php';

	if ( ! file_exists( $woocommerce_main_file ) ) {
		fwrite(
			STDERR,
			"WooCommerce not found at {$woocommerce_main_file} - is it listed in .wp-env.json's " .
			"\"plugins\" array and did `wp-env start` finish successfully?\n"
		);
		exit( 1 );
	}

	require $woocommerce_main_file;
	require dirname( __DIR__ ) . '/moneropay-cloud.php';
}
tests_add_filter( 'muplugins_loaded', '_moneropay_cloud_manually_load_plugins' );

// Hands off to the real WP core test bootstrap: installs a fresh test
// database, finishes loading WordPress (firing the `muplugins_loaded` hook
// registered above along the way), and makes `WP_UnitTestCase` and friends
// available to every test file PHPUnit collects from `tests/`.
require $_tests_dir . '/includes/bootstrap.php';
