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

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
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
    /// An engine webhook (`webhooks.id`).
    WebhookId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_its_string_in_json_and_sqlite() {
        let id = OrderId::new("order_1");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"order_1\"");
        assert_eq!(serde_json::from_str::<OrderId>("\"order_1\"").unwrap(), id);
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let back: OrderId = conn
            .query_row("SELECT ?1", [&id], |row| row.get(0))
            .unwrap();
        assert_eq!(back, id);
        assert_eq!(id.to_string(), "order_1");
    }
}
