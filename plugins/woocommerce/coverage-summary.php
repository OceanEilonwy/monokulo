<?php
// Read PHPUnit's native serialized CodeCoverage object, including Xdebug's
// branch/path data. Clover is retained for line exchange, not branch totals.
require __DIR__ . '/vendor/autoload.php';

$coverage = require '/coverage/coverage.php';
if ( ! $coverage instanceof SebastianBergmann\CodeCoverage\CodeCoverage ) {
	fwrite( STDERR, "PHPUnit coverage object is unavailable\n" );
	exit( 1 );
}
$root = $coverage->getReport();
$files = array();
$visit = static function ( $node ) use ( &$visit, &$files ) {
	foreach ( $node->files() as $file ) {
		$files[] = array(
			'name'     => $file->name(),
			'lines'    => array( 'covered' => $file->numberOfExecutedLines(), 'total' => $file->numberOfExecutableLines() ),
			'branches' => array( 'covered' => $file->numberOfExecutedBranches(), 'total' => $file->numberOfExecutableBranches() ),
			'paths'    => array( 'covered' => $file->numberOfExecutedPaths(), 'total' => $file->numberOfExecutablePaths() ),
		);
	}
	foreach ( $node->directories() as $directory ) {
		$visit( $directory );
	}
};
$visit( $root );
$result = array(
	'versions' => array(
		'php'     => PHP_VERSION,
		'phpunit' => PHPUnit\Runner\Version::id(),
		'xdebug'  => phpversion( 'xdebug' ),
	),
	'lines'    => array( 'covered' => $root->numberOfExecutedLines(), 'total' => $root->numberOfExecutableLines() ),
	'branches' => array( 'covered' => $root->numberOfExecutedBranches(), 'total' => $root->numberOfExecutableBranches() ),
	'paths'    => array( 'covered' => $root->numberOfExecutedPaths(), 'total' => $root->numberOfExecutablePaths() ),
	'files'    => $files,
);
if ( $result['lines']['total'] <= 0 || $result['branches']['total'] <= 0 || $result['paths']['total'] <= 0 ) {
	fwrite( STDERR, "PHPUnit did not collect nonzero line, branch, and path denominators\n" );
	exit( 1 );
}
file_put_contents( '/coverage/summary.json', json_encode( $result, JSON_PRETTY_PRINT | JSON_THROW_ON_ERROR ) );
