-- Runtime-configurable settings (the new admin HTTP API, `docs/order_rescan_wbs.md`
-- being an unrelated example of the kind of thing that used to be TOML-only). A
-- plain key/value table rather than one column per setting: settings are read and
-- written by name from Rust (`shared::settings::resolve_parsed`, fed a value read
-- from this table by key), never queried by value or joined against, so there is no
-- relational structure here to actually model - and a new setting never needs a new
-- migration to add a column for it, just a new key someone starts reading/writing.
--
-- `value` is always a string - the same "everything is text, the reader parses it"
-- convention `scanner.rs`'s own former TOML config used for numeric/boolean fields
-- read out of a text file. A row's absence (not a NULL or empty-string value) means
-- "nothing has ever been saved for this setting" - see `shared::settings`'s own
-- module doc comment for the env > database > default precedence this enables.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
