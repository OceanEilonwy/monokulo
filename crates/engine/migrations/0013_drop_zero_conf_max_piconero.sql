-- Native 0-conf: `confirmations_required = 0` is now a legal value on the
-- confirmation ladder itself (`status::derive_status`'s own doc comment explains
-- why it needs no special-casing to be safe), which makes this tenant-wide,
-- tier-blind amount ceiling entirely redundant - and worse, since it trusted an
-- amount regardless of which confirmation tier (if any) the order belonged to.
--
-- Preserve the effective policy of orders already settled by the old ceiling.
-- Without this, the next scan could retract a paid status solely because the
-- setting disappeared. New orders use a zero-confirmation tier explicitly.
UPDATE orders
SET confirmations_required_override = 0
WHERE id IN (
    SELECT o.id
    FROM orders o JOIN tenants t ON t.id = o.tenant_id
    WHERE t.zero_conf_max_piconero IS NOT NULL
      AND o.status IN ('paid', 'overpaid')
      AND o.amount_received_piconero <= t.zero_conf_max_piconero
      AND o.confirmations < COALESCE(o.confirmations_required_override, t.confirmations_required)
);

-- Same as migration 0005: no index, `CHECK`, or foreign-key constraint on this
-- column, so no table rebuild is needed.
ALTER TABLE tenants DROP COLUMN zero_conf_max_piconero;
