-- `http_cache.max_mb` sized a response cache that is gone: no engine
-- response was marked cacheable, and the exchange-rate providers keep
-- their own caches. Nothing reads its row, kept from when settings were
-- saved here; it goes so the Query page and backups stop showing it.
DELETE FROM settings WHERE key = 'http_cache.max_mb';
