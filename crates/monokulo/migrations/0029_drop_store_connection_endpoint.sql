-- Each store recorded the engine address it was connected through
-- (`moneropay_endpoint`, from `EngineClient`'s URL), but nothing ever read
-- it: every store reaches the one engine monokulo is configured with, and
-- with the engine inside monokulo (docs/engine_as_library.md) there is no
-- address to record. Dropped rather than kept filling with a value that
-- means nothing.
ALTER TABLE store_connections DROP COLUMN moneropay_endpoint;
