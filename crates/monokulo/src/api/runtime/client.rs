//! Who a machine client says it is: `Monokulo-Client: name/version`. A
//! shop plugin sends its slug (`prestashop/1.2.3`), an app its own; the API
//! records it on the orders it makes (`super::extract::Client`), and the
//! plugin and app handshakes keep the version they connected with.

/// The header a client names itself in, `name/version`.
pub const CLIENT_HEADER: &str = "monokulo-client";

/// The version a request's [`CLIENT_HEADER`] gives for the client `name`
/// (any case): `prestashop/0.4.0` is version `0.4.0` of `prestashop`.
/// `None` when the header names another client, or none.
pub(crate) fn client_version(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(CLIENT_HEADER)?.to_str().ok()?;
    let (client, version) = value.split_once('/').unwrap_or((value, ""));
    client
        .trim()
        .eq_ignore_ascii_case(name)
        .then(|| version.trim().chars().take(40).collect())
}

/// The client a request's [`CLIENT_HEADER`] names and its version (empty
/// when it gave none, at most 40 characters).
pub(crate) fn client(headers: &axum::http::HeaderMap) -> Option<(String, String)> {
    let value = headers.get(CLIENT_HEADER)?.to_str().ok()?;
    let (name, version) = value.split_once('/').unwrap_or((value, ""));
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_owned(), version.trim().chars().take(40).collect()))
}
