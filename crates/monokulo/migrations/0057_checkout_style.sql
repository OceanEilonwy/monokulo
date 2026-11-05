-- A store's checkout colours and shape (`views::checkout_style`), as JSON: empty
-- is the theme's own, which every existing store keeps.
ALTER TABLE store_connections ADD COLUMN checkout_style TEXT NOT NULL DEFAULT '{}';

-- Stylesheets a checkout page links by the hash of their content
-- (`GET /static-style/{hash}.css`), kept a while after they're replaced for pages
-- still open.
CREATE TABLE checkout_stylesheets (
    hash            TEXT NOT NULL,
    connection_id   TEXT NOT NULL REFERENCES store_connections (id),
    css             TEXT NOT NULL,
    created_at_utc  INTEGER NOT NULL,
    -- Two stores with the same colours have the same hash, and each its own row,
    -- so one going can't take the other's stylesheet with it.
    PRIMARY KEY (hash, connection_id)
);
CREATE INDEX checkout_stylesheets_by_store ON checkout_stylesheets (connection_id);
