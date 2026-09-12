-- Links a control-plane user to a tenant provisioned on a real engine
-- instance (WBS 1.2.2). `tenant_public_key` comes straight from the engine's
-- `CreateTenantResponse.public_key` - see `EngineClient::create_tenant`.
--
-- `tenant_secret_token_encrypted` holds the engine's `secret_token`
-- encrypted at rest (WBS 1.2.3): AES-256-GCM via `crate::crypto::encrypt`,
-- called at the HTTP handler layer (`http/connections.rs`) before the row is
-- ever inserted - this table (and `Db` generally) never sees the raw
-- `sk_...` value. Still a plain `TEXT` column: the encrypted form is a
-- hex-encoded nonce+ciphertext string, no schema change needed from when
-- this column briefly held the plaintext value (WBS 1.2.2).
CREATE TABLE store_connections (
    id                          TEXT PRIMARY KEY,
    user_id                     TEXT NOT NULL REFERENCES users(id),
    platform                    TEXT NOT NULL,
    site_url                    TEXT NOT NULL,
    tenant_public_key           TEXT NOT NULL,
    tenant_secret_token_encrypted TEXT NOT NULL,
    moneropay_endpoint          TEXT NOT NULL,
    created_at                  INTEGER NOT NULL
);
