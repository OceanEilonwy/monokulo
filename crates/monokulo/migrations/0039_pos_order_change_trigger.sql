-- Announces every change to what a POS order's live stream shows (whether
-- it is backgrounded or cancelled), whichever statement makes it.
-- `pos_order_changed` is not built into SQLite: monokulo defines it on its
-- writing connection (`db::order_changes`), where it notes the order, and
-- the order's streams are woken once the transaction has committed. On a
-- connection without it, a statement that fires this trigger fails, so no
-- writer can change these columns without the streams hearing of it.
CREATE TRIGGER pos_order_changed AFTER UPDATE OF backgrounded, cancelled_at_utc ON pos_orders
WHEN OLD.backgrounded IS NOT NEW.backgrounded OR OLD.cancelled_at_utc IS NOT NEW.cancelled_at_utc
BEGIN
    SELECT pos_order_changed(NEW.connection_id, NEW.order_id);
END;
