-- Deliveries by when they were made: the engine page's webhook rate, a few
-- minutes' worth read every 10 s (docs/engine_visualizer.md). Checked by a
-- query-plan test (store::work::tests).
CREATE INDEX webhook_deliveries_delivered_idx ON webhook_deliveries (delivered_at_utc)
    WHERE delivered_at_utc IS NOT NULL;
