-- A retired wallet (docs/wallets.md, "Retiring a wallet"): no store can
-- use it, it isn't offered anywhere, and its keys are deleted, in the
-- engine and its key storage. Its name, address and history stay, for the
-- stores that used it, and it can be brought back with its keys.
ALTER TABLE wallets ADD COLUMN retired_at_utc INTEGER;
