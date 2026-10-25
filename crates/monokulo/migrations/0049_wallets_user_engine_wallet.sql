-- A wallet here stands for one wallet in the engine, and one account has
-- one wallet here for it: a store made before wallets is linked by the
-- engine wallet it is on (`http::wallet_service::adopt_unlinked_stores`),
-- so what the engine says about a wallet is about the stores its page lists.
CREATE UNIQUE INDEX wallets_user_engine_wallet ON wallets (user_id, engine_wallet_id);
