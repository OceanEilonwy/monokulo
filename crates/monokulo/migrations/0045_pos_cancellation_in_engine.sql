-- A POS cancellation is the engine's now (its `cancelled` status, and the
-- order's `cancelled_at`): the checkout, the dashboard, the API, webhooks
-- and apps all read it from there. monokulo's own copy goes, so there is
-- one record of it. An order the POS cancelled before this shows as the
-- engine has it: expired, as it was once its deadline passed.
DROP TRIGGER pos_order_changed;
ALTER TABLE pos_orders DROP COLUMN cancelled_at_utc;

-- Announces every change to what a POS order's live stream shows from
-- here (whether it is backgrounded), whichever statement makes it. As
-- migration 0039 describes, `pos_order_changed` is monokulo's own function.
CREATE TRIGGER pos_order_changed AFTER UPDATE OF backgrounded ON pos_orders
WHEN OLD.backgrounded IS NOT NEW.backgrounded
BEGIN
    SELECT pos_order_changed(NEW.connection_id, NEW.order_id);
END;
