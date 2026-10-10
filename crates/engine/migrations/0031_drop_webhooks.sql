-- The engine no longer sends webhooks (docs/DESIGN.md §11): monokulo reads
-- the order-event log (migration 0030) and delivers each store's webhooks
-- itself, from its own tables. Not in production, so nothing to carry over.
DROP INDEX IF EXISTS webhook_deliveries_due_idx;
DROP INDEX IF EXISTS webhook_deliveries_delivered_idx;
DROP INDEX IF EXISTS webhooks_tenant_idx;
DROP TABLE webhook_deliveries;
DROP TABLE webhooks;
