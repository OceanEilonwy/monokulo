<?php
// Applied before PHPUnit's bootstrap so Xdebug never instruments WordPress,
// WooCommerce, Composer, or test files while collecting branch paths.
if ( ! extension_loaded( 'xdebug' ) || ! in_array( 'coverage', xdebug_info( 'mode' ), true ) ) {
	fwrite( STDERR, "missing prerequisite: Xdebug coverage mode\n" );
	exit( 2 );
}
xdebug_set_filter(
	XDEBUG_FILTER_CODE_COVERAGE,
	XDEBUG_PATH_INCLUDE,
	array( __DIR__ . '/monokulo.php', __DIR__ . '/includes/' )
);
