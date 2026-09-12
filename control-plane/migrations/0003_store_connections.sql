-- Links a control-plane user to a tenant provisioned on a real engine
-- instance (WBS 1.2.2). `tenant_public_key`/`tenant_secret_token_encrypted`
-- come straight from the engine's `CreateTenantResponse`
-- (`public_key`/`secret_token`) - see `EngineClient::create_tenant`.
--
-- IMPORTANT: despite its name, `tenant_secret_token_encrypted` currently
-- holds the engine's raw `sk_...` secret token, UNENCRYPTED. The column is
-- named for its final, intended shape (WBS 1.2.3 adds real encryption at
-- rest) so that task doesn't need a rename migration - but as of this
-- migration, treat this column as plaintext-sensitive: it is a live
-- server-to-server credential for the tenant it names, not yet protected
-- by anything beyond normal database access control.
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
