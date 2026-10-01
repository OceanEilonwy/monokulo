-- The engine is private: only monokulo talks to it, through the `sk_`-
-- authenticated admin API. Its public `/api/v1/t/{pk}/...` routes, their
-- CORS layer and the per-tenant origin check that read this column are gone,
-- and which websites may embed a store's checkout is now monokulo's concern
-- alone (its verified embed domains). Nothing reads or writes this column
-- any more.
--
-- Same as migrations 0005/0006/0013: no index, `CHECK` or foreign key uses
-- this column, so a plain `DROP COLUMN` is enough - no table rebuild.
ALTER TABLE tenants DROP COLUMN allowed_origins;
