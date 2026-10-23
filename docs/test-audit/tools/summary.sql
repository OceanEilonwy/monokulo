.headers on
.mode column
SELECT 'unreviewed (non-property)' what, count(*) n FROM test WHERE test_type <> 'property' AND value IS NULL;
SELECT scope, value, count(*) n, round(sum(duration_ms)/1000,1) serial_s FROM test WHERE value IS NOT NULL GROUP BY 1,2 ORDER BY 1,2;
SELECT test_type, count(*) n, round(sum(duration_ms)/1000,1) serial_s FROM test GROUP BY 1 ORDER BY 2 DESC;
SELECT count(*) modules FROM module;
SELECT count(*) overlaps FROM test_overlap;
SELECT source, count(*) suggestions, round(sum(estimated_ci_speedup),1) speedup_s, round(sum(estimated_coverage_impact),2) cov_pp FROM suggestion GROUP BY 1;
SELECT count(*) tests_without_module FROM test t WHERE test_type <> 'property' AND NOT EXISTS (SELECT 1 FROM test_modules tm WHERE tm.test_id = t.id);
