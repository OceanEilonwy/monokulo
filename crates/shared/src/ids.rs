//! Identifiers as types. A method that takes two ids (a store and an
//! order, say) takes two different types, so passing them in the wrong
//! order doesn't compile. Each is a string underneath: stored as TEXT,
//! serialized as a JSON string, shown as itself.

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                $name(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string" })
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                $name(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                $name(value.to_string())
            }
        }

        /// Comparing with text is exact; it can't mix up two kinds of id.
        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                String::column_result(value).map($name)
            }
        }
    };
}

id!(
    /// A monokulo store connection (`store_connections.id`).
    ConnectionId
);
id!(
    /// A monokulo account (`users.id`).
    UserId
);
id!(
    /// An order: the engine's `orders.id`, which monokulo keys its own
    /// per-order records by.
    OrderId
);
id!(
    /// An engine tenant (`tenants.id`).
    TenantId
);
id!(
    /// A store's webhook (monokulo's `webhooks.id`).
    WebhookId
);
id!(
    /// A merchant's named wallet in monokulo (`wallets.id`).
    WalletId
);
id!(
    /// The engine's record of a wallet's keys (the engine's `wallets.id`),
    /// which stores on that wallet are created from.
    EngineWalletId
);
id!(
    /// An app connected to a store (monokulo's `app_connections.id`).
    AppId
);
