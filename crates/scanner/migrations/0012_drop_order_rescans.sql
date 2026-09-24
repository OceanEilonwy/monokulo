-- `docs/txid_lookup_and_scan_chunking_wbs.md` Part C.1: the manual chain-rescan
-- feature (`docs/order_rescan_wbs.md`, migration 0007) is removed entirely,
-- replaced by a direct look-up-by-txid action that needs no durable job state at
-- all (a single synchronous request/response, not a resumable background job).
-- Dropping the table also drops its two indexes.
--
-- `orders.first_scanned_height`/`last_scanned_height` (migration 0008) are kept -
-- still populated by ordinary live scanning alone, still meaningful without a
-- rescan feature ("has this order's address ever been checked, how recently").
DROP TABLE order_rescans;
