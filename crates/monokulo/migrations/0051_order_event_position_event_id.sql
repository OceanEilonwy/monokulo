-- The id of the event `order_event_position.after_seq` names, as read: the
-- engine tells by it whether its event at that number is the one read, or
-- another one its database, restored from a backup, handed the same
-- number. NULL when the position isn't an event read (one the engine said
-- to carry on from, or an event that couldn't be read); the existing row
-- starts so, and the next event read fills it in.
ALTER TABLE order_event_position ADD COLUMN after_event_id TEXT;
