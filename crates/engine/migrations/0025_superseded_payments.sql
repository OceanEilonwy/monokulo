-- Payments sharing a one-time output key (the "burning bug": only one output
-- with a key can ever be spent) are all recorded; once one of them is in a
-- block (a proven one, while proof-of-work checking is on), it is the one
-- credited, and the others are voided with its id here. Unlike a double
-- spend's void this one is undone if the credited payment loses its block:
-- every recompute of the order settles it again (`store::conflicts`).
ALTER TABLE order_payments ADD COLUMN superseded_by INTEGER;
