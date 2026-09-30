//! The Monero nodes form on the admin settings page (nicer_admin_screen.md
//! step 5): plain fields, one row per node, instead of a box of JSON.
//!
//! The engine still stores and takes a network's nodes as one JSON value
//! (`MoneroNodeSetting`: a primary with an ordered list of `fallbacks`); the
//! form is only the page's way of writing it. Everything here is pure:
//! turning the submitted `node_<network>_<index>_<field>` fields into
//! ordered rows, applying a row button (`node_action`), checking addresses,
//! and converting rows to that JSON and back. The save handler
//! (`http::admin_settings`) and the view (`views::admin`) do the rest.
//!
//! Row buttons are submit buttons of the tab's form, so every change to the
//! list is a normal save and works without JavaScript: the submitted rows
//! are the list, in order, and the button says what to do to it first.

use std::collections::{HashMap, HashSet};

/// The networks the engine can scan, in the order the page shows them.
pub const NETWORKS: [&str; 3] = ["mainnet", "stagenet", "testnet"];

/// One node row as submitted (or as saved): what the admin typed and
/// ticked, and what's wrong with it, if anything.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeRow {
    /// As typed: `host:port`, optionally with `http://` or `https://`.
    pub address: String,
    /// "Use TLS".
    pub ssl: bool,
    /// "Accept a self-signed certificate".
    pub self_signed: bool,
    /// Shown under the address; nothing is saved while any row has one.
    pub error: Option<String>,
}

impl NodeRow {
    /// A row with nothing in its address, which a save ignores: the blank
    /// "Add a node" row, or a row the admin emptied.
    pub fn is_blank(&self) -> bool {
        self.address.trim().is_empty()
    }
}

/// A node address, checked and split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAddress {
    /// The host as the engine connects to it: a name, an IPv4 address, or
    /// an IPv6 address in its brackets (`[::1]`), since the engine puts the
    /// host straight into its `http://host:port` URL.
    pub host: String,
    pub port: u16,
    /// The address was typed with `https://`: TLS is ticked on save.
    pub https: bool,
}

impl NodeAddress {
    /// `host:port`, the engine's label for this node on `/status`.
    pub fn label(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Checks and splits an address as typed (the rules in the work pack's
/// section 2): `host:port`, `http://host:port` or `https://host:port`; an
/// IPv6 address in brackets, `[::1]:18081`. The port is always needed.
pub fn parse_address(input: &str) -> Result<NodeAddress, String> {
    let input = input.trim();
    let lower = input.to_ascii_lowercase();
    let (rest, https) = if lower.starts_with("https://") {
        (&input["https://".len()..], true)
    } else if lower.starts_with("http://") {
        (&input["http://".len()..], false)
    } else if input.contains("://") {
        return Err("Start the address with http:// or https://, or leave that out.".to_string());
    } else {
        (input, false)
    };
    // A pasted URL's trailing slash is harmless; anything after it isn't
    // part of a node's address.
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains(['/', '?', '#']) {
        return Err(
            "Leave out the path: just the host and port, like node.example.com:18081.".to_string(),
        );
    }
    if rest.contains(char::is_whitespace) || rest.contains('@') {
        return Err(
            "An address has no spaces or @: just the host and port, like node.example.com:18081."
                .to_string(),
        );
    }
    let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
        let Some((ip, after)) = bracketed.split_once(']') else {
            return Err("An IPv6 address needs its closing bracket, like [::1]:18081.".to_string());
        };
        if ip.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(
                "That isn't an IPv6 address between the brackets. Use one like [::1]:18081."
                    .to_string(),
            );
        }
        let Some(port) = after.strip_prefix(':') else {
            return Err("Add the port after the brackets, like [::1]:18081.".to_string());
        };
        (format!("[{ip}]"), port)
    } else {
        if rest.contains(']') {
            return Err("An IPv6 address needs its opening bracket, like [::1]:18081.".to_string());
        }
        let Some((host, port)) = rest.rsplit_once(':') else {
            return Err("Add the port, like node.example.com:18081.".to_string());
        };
        if host.contains(':') {
            return Err("Put an IPv6 address in brackets, like [::1]:18081.".to_string());
        }
        if host.is_empty() {
            return Err("Enter the node's host name or IP address before the port, like node.example.com:18081.".to_string());
        }
        (host.to_string(), port)
    };
    let digits = port;
    let port = match digits.parse::<u16>() {
        Ok(port) if port > 0 && digits.chars().all(|c| c.is_ascii_digit()) => port,
        _ => return Err("The port must be a number from 1 to 65535.".to_string()),
    };
    Ok(NodeAddress { host, port, https })
}

/// `host:port` for a saved node, as the address box shows it. A bare IPv6
/// host (saved through the API without brackets) gets its brackets back.
pub fn format_address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The networks' rows as submitted, in the page's order of networks, each
/// network's rows in the order of their index. Only networks with at least
/// one submitted row are here: a tab without the node form sends none.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeForm {
    pub networks: Vec<(String, Vec<NodeRow>)>,
}

/// A row button: what to do to the submitted rows before saving them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeAction {
    Remove { network: String, index: usize },
    Up { network: String, index: usize },
    Down { network: String, index: usize },
}

impl NodeAction {
    /// `remove:<network>:<index>`, `up:<network>:<index>` or
    /// `down:<network>:<index>`, the value of the button pressed.
    pub fn parse(value: &str) -> Option<NodeAction> {
        let mut parts = value.split(':');
        let (verb, network, index) = (
            parts.next()?,
            parts.next()?.to_string(),
            parts.next()?.parse().ok()?,
        );
        if parts.next().is_some() || network.is_empty() {
            return None;
        }
        match verb {
            "remove" => Some(NodeAction::Remove { network, index }),
            "up" => Some(NodeAction::Up { network, index }),
            "down" => Some(NodeAction::Down { network, index }),
            _ => None,
        }
    }

    /// The button's value: the inverse of [`NodeAction::parse`].
    pub fn value(&self) -> String {
        match self {
            NodeAction::Remove { network, index } => format!("remove:{network}:{index}"),
            NodeAction::Up { network, index } => format!("up:{network}:{index}"),
            NodeAction::Down { network, index } => format!("down:{network}:{index}"),
        }
    }
}

/// One `node_<network>_<index>_<field>` form name, split.
fn node_field(name: &str) -> Option<(&str, usize, &str)> {
    let rest = name.strip_prefix("node_")?;
    let (network, rest) = rest.split_once('_')?;
    let (index, field) = rest.split_once('_')?;
    if network.is_empty() || !network.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some((network, index.parse().ok()?, field))
}

/// Whether a form field belongs to the node form: a row field or a row
/// button.
pub fn is_node_field(name: &str) -> bool {
    name == "node_action" || node_field(name).is_some()
}

impl NodeForm {
    /// The rows in a submitted form, with the row button pressed (if any)
    /// applied, blank rows dropped, and every remaining row checked: its
    /// address, and that it isn't listed twice in its network. `order` is
    /// the order networks are shown in; any other network submitted comes
    /// after them. A checkbox that isn't ticked isn't sent, so a row's boxes
    /// are ticked only when their field is there.
    pub fn from_form(form: &HashMap<String, String>, order: &[&str]) -> NodeForm {
        let mut by_network: HashMap<&str, Vec<(usize, NodeRow)>> = HashMap::new();
        for (name, value) in form {
            let Some((network, index, "address")) = node_field(name) else {
                continue;
            };
            let ticked =
                |field: &str| form.contains_key(&format!("node_{network}_{index}_{field}"));
            by_network.entry(network).or_default().push((
                index,
                NodeRow {
                    address: value.trim().to_string(),
                    ssl: ticked("ssl"),
                    self_signed: ticked("self_signed"),
                    error: None,
                },
            ));
        }
        let mut names: Vec<&str> = by_network.keys().copied().collect();
        names.sort_by_key(|name| {
            (
                order.iter().position(|o| o == name).unwrap_or(usize::MAX),
                name.to_string(),
            )
        });
        let action = form
            .get("node_action")
            .and_then(|value| NodeAction::parse(value));
        let networks = names
            .into_iter()
            .map(|network| {
                let mut rows = by_network.remove(network).unwrap_or_default();
                rows.sort_by_key(|(index, _)| *index);
                let rows: Vec<(usize, NodeRow)> = rows
                    .into_iter()
                    .filter(|(_, row)| !row.is_blank())
                    .collect();
                let mut rows = match &action {
                    Some(action) => apply_action(rows, network, action),
                    None => rows.into_iter().map(|(_, row)| row).collect(),
                };
                check_rows(&mut rows);
                (network.to_string(), rows)
            })
            .collect();
        NodeForm { networks }
    }

    /// Whether any row has something to fix.
    pub fn has_errors(&self) -> bool {
        self.networks
            .iter()
            .any(|(_, rows)| rows.iter().any(|row| row.error.is_some()))
    }

    /// One network's rows, if it was submitted.
    pub fn rows(&self, network: &str) -> Option<&[NodeRow]> {
        self.networks
            .iter()
            .find(|(n, _)| n == network)
            .map(|(_, rows)| rows.as_slice())
    }
}

/// Applies a row button to one network's rows (each with its submitted
/// index). A button for another network, or for a row that isn't there
/// (a blank one), changes nothing; so do Up on the first row and Down on
/// the last.
fn apply_action(rows: Vec<(usize, NodeRow)>, network: &str, action: &NodeAction) -> Vec<NodeRow> {
    let position = |index: usize| rows.iter().position(|(i, _)| *i == index);
    let (at, verb) = match action {
        NodeAction::Remove { network: n, index } if n == network => (position(*index), 'r'),
        NodeAction::Up { network: n, index } if n == network => (position(*index), 'u'),
        NodeAction::Down { network: n, index } if n == network => (position(*index), 'd'),
        _ => (None, ' '),
    };
    let mut rows: Vec<NodeRow> = rows.into_iter().map(|(_, row)| row).collect();
    match (at, verb) {
        (Some(at), 'r') => {
            rows.remove(at);
        }
        (Some(at), 'u') if at > 0 => rows.swap(at, at - 1),
        (Some(at), 'd') if at + 1 < rows.len() => rows.swap(at, at + 1),
        _ => {}
    }
    rows
}

/// Checks each row's address, and that no address is listed twice in the
/// network (the second one says so).
fn check_rows(rows: &mut [NodeRow]) {
    let mut seen = HashSet::new();
    for row in rows.iter_mut() {
        match parse_address(&row.address) {
            Ok(address) => {
                if !seen.insert((address.host.to_ascii_lowercase(), address.port)) {
                    row.error = Some("This node is already listed above.".to_string());
                }
            }
            Err(message) => row.error = Some(message),
        }
    }
}

/// A network's rows as the engine's `monero_node.<network>` JSON: the first
/// row is the primary, the rest its `fallbacks`, in order; `None` (clear the
/// network) when there are no rows. `None` too for rows with errors, which
/// a caller checks for first.
pub fn rows_to_setting(rows: &[NodeRow]) -> Option<serde_json::Value> {
    let nodes: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let address = parse_address(&row.address).ok()?;
            Some(serde_json::json!({
                "host": address.host,
                "port": address.port,
                "ssl": row.ssl || address.https,
                "accept_self_signed_certs": row.self_signed,
                "fallbacks": [],
            }))
        })
        .collect::<Option<_>>()?;
    let mut nodes = nodes.into_iter();
    let mut primary = nodes.next()?;
    primary["fallbacks"] = serde_json::Value::Array(nodes.collect());
    Some(primary)
}

/// A saved node as the engine stores it: `ssl` defaults to off and
/// `accept_self_signed_certs` to on, as in `MoneroNodeSetting`.
fn saved_node(value: &serde_json::Value) -> Option<(String, u16, NodeRow)> {
    let host = value.get("host")?.as_str()?.to_string();
    let port = u16::try_from(value.get("port")?.as_u64()?).ok()?;
    let row = NodeRow {
        address: format_address(&host, port),
        ssl: value
            .get("ssl")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        self_signed: value
            .get("accept_self_signed_certs")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        error: None,
    };
    Some((host, port, row))
}

/// A saved `monero_node.<network>` value as rows, primary first, each with
/// the engine's label for it (`host:port`, as saved) to match its
/// `/status`. A fallback's own `fallbacks` are dropped: the engine never
/// uses them, and the form has no place for them.
pub fn rows_from_setting(value: Option<&serde_json::Value>) -> Vec<(String, NodeRow)> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Vec::new();
    };
    let fallbacks = value
        .get("fallbacks")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    std::iter::once(value)
        .chain(fallbacks.iter())
        .filter_map(saved_node)
        .map(|(host, port, row)| (format!("{host}:{port}"), row))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(host: &str, port: u16, https: bool) -> NodeAddress {
        NodeAddress {
            host: host.to_string(),
            port,
            https,
        }
    }

    #[test]
    fn addresses_are_host_and_port_with_an_optional_scheme() {
        assert_eq!(
            parse_address("node.example.com:18081"),
            Ok(address("node.example.com", 18081, false))
        );
        assert_eq!(
            parse_address("  node.example.com:18081  "),
            Ok(address("node.example.com", 18081, false))
        );
        assert_eq!(
            parse_address("http://node.example.com:18081"),
            Ok(address("node.example.com", 18081, false))
        );
        assert_eq!(
            parse_address("HTTPS://node.example.com:18089/"),
            Ok(address("node.example.com", 18089, true))
        );
        assert_eq!(
            parse_address("127.0.0.1:1"),
            Ok(address("127.0.0.1", 1, false))
        );
        assert_eq!(
            parse_address("[::1]:18081"),
            Ok(address("[::1]", 18081, false))
        );
        assert_eq!(
            parse_address("https://[2001:db8::1]:443"),
            Ok(address("[2001:db8::1]", 443, true))
        );
        assert_eq!(parse_address("[::1]:18081").unwrap().label(), "[::1]:18081");
    }

    #[test]
    fn a_bad_address_says_what_to_fix() {
        let error = |input: &str| parse_address(input).unwrap_err();
        assert!(
            error("node.example.com").contains("Add the port"),
            "missing port"
        );
        assert!(
            error("https://node.example.com").contains("Add the port"),
            "the port is needed with a scheme too"
        );
        assert!(error("node.example.com:0").contains("from 1 to 65535"));
        assert!(error("node.example.com:65536").contains("from 1 to 65535"));
        assert!(error("node.example.com:port").contains("from 1 to 65535"));
        assert!(error("node.example.com:+80").contains("from 1 to 65535"));
        assert!(
            error(":18081").contains("host name or IP address"),
            "empty host"
        );
        assert!(error("[::1:18081").contains("closing bracket"));
        assert!(error("::1]:18081").contains("opening bracket"));
        assert!(error("[::1]18081").contains("port after the brackets"));
        assert!(error("[not-ipv6]:18081").contains("isn't an IPv6 address"));
        assert!(error("::1:18081").contains("in brackets"), "bare IPv6");
        assert!(
            error("node.example.com:18081/json_rpc").contains("Leave out the path"),
            "a stray path"
        );
        assert!(error("http://node.example.com:18081/get_info?x=1").contains("Leave out the path"));
        assert!(error("ftp://node.example.com:21").contains("http:// or https://"));
        assert!(error("node example.com:18081").contains("no spaces"));
        assert!(error("user@node.example.com:18081").contains("no spaces or @"));
    }

    fn form(fields: &[(&str, &str)]) -> HashMap<String, String> {
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const ORDER: &[&str] = &["mainnet", "stagenet", "testnet"];

    fn addresses(form: &NodeForm, network: &str) -> Vec<String> {
        form.rows(network)
            .unwrap()
            .iter()
            .map(|row| row.address.clone())
            .collect()
    }

    /// Three stagenet rows plus the blank "Add a node" row, as the page
    /// sends them.
    fn three_rows(extra: &[(&str, &str)]) -> HashMap<String, String> {
        let mut fields = vec![
            ("node_stagenet_0_address", "a.example:1"),
            ("node_stagenet_0_self_signed", "on"),
            ("node_stagenet_1_address", "b.example:2"),
            ("node_stagenet_1_ssl", "on"),
            ("node_stagenet_2_address", "c.example:3"),
            ("node_stagenet_3_address", ""),
            ("node_stagenet_3_self_signed", "on"),
        ];
        fields.extend_from_slice(extra);
        form(&fields)
    }

    #[test]
    fn submitted_rows_keep_their_order_and_boxes_and_the_blank_row_is_ignored() {
        let nodes = NodeForm::from_form(
            &three_rows(&[
                ("node_mainnet_0_address", ""),
                ("payment.x", "1"),
                ("tab", "nodes"),
            ]),
            ORDER,
        );
        assert_eq!(
            nodes
                .networks
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["mainnet", "stagenet"]
        );
        assert!(
            nodes.rows("mainnet").unwrap().is_empty(),
            "only the blank row: an empty list"
        );
        let rows = nodes.rows("stagenet").unwrap();
        assert_eq!(
            addresses(&nodes, "stagenet"),
            ["a.example:1", "b.example:2", "c.example:3"]
        );
        assert!(!rows[0].ssl && rows[0].self_signed);
        assert!(rows[1].ssl && !rows[1].self_signed);
        assert!(!nodes.has_errors());
        assert_eq!(nodes.rows("testnet"), None, "not submitted");
    }

    #[test]
    fn rows_are_ordered_by_their_index_not_the_forms_order() {
        let nodes = NodeForm::from_form(
            &form(&[
                ("node_testnet_10_address", "late.example:1"),
                ("node_testnet_2_address", "early.example:1"),
            ]),
            ORDER,
        );
        assert_eq!(
            addresses(&nodes, "testnet"),
            ["early.example:1", "late.example:1"]
        );
    }

    #[test]
    fn every_row_button_including_at_the_edges() {
        let with = |action: &str| {
            addresses(
                &NodeForm::from_form(&three_rows(&[("node_action", action)]), ORDER),
                "stagenet",
            )
        };
        assert_eq!(with("remove:stagenet:1"), ["a.example:1", "c.example:3"]);
        assert_eq!(
            with("up:stagenet:2"),
            ["a.example:1", "c.example:3", "b.example:2"]
        );
        assert_eq!(
            with("down:stagenet:0"),
            ["b.example:2", "a.example:1", "c.example:3"]
        );
        assert_eq!(
            with("up:stagenet:0"),
            ["a.example:1", "b.example:2", "c.example:3"],
            "up on the first row"
        );
        assert_eq!(
            with("down:stagenet:2"),
            ["a.example:1", "b.example:2", "c.example:3"],
            "down on the last row"
        );
        assert_eq!(
            with("down:stagenet:3"),
            ["a.example:1", "b.example:2", "c.example:3"],
            "the blank row has no buttons"
        );
        assert_eq!(
            with("remove:mainnet:0"),
            ["a.example:1", "b.example:2", "c.example:3"],
            "another network's button"
        );
        assert_eq!(
            with("nonsense"),
            ["a.example:1", "b.example:2", "c.example:3"]
        );
        // A moved row takes its boxes with it.
        let moved = NodeForm::from_form(&three_rows(&[("node_action", "up:stagenet:1")]), ORDER);
        assert!(moved.rows("stagenet").unwrap()[0].ssl);

        let only = form(&[
            ("node_testnet_0_address", "only.example:1"),
            ("node_testnet_1_address", ""),
            ("node_action", "remove:testnet:0"),
        ]);
        assert!(
            NodeForm::from_form(&only, ORDER)
                .rows("testnet")
                .unwrap()
                .is_empty(),
            "removing the only row leaves none"
        );
    }

    #[test]
    fn a_button_value_round_trips() {
        for action in [
            NodeAction::Remove {
                network: "stagenet".into(),
                index: 2,
            },
            NodeAction::Up {
                network: "mainnet".into(),
                index: 1,
            },
            NodeAction::Down {
                network: "testnet".into(),
                index: 0,
            },
        ] {
            assert_eq!(NodeAction::parse(&action.value()), Some(action));
        }
        for bad in [
            "remove:stagenet",
            "up::1",
            "down:testnet:x",
            "swap:testnet:1",
            "up:testnet:1:2",
        ] {
            assert_eq!(NodeAction::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn bad_and_repeated_addresses_are_marked_on_their_own_row() {
        let nodes = NodeForm::from_form(
            &form(&[
                ("node_mainnet_0_address", "node.example.com:18081"),
                ("node_mainnet_1_address", "node.example.com"),
                ("node_mainnet_2_address", "NODE.example.com:18081"),
                ("node_mainnet_3_address", "other.example.com:18081"),
            ]),
            ORDER,
        );
        let rows = nodes.rows("mainnet").unwrap();
        assert!(nodes.has_errors());
        assert_eq!(
            rows[0].error, None,
            "the first of a repeated address is fine"
        );
        assert!(rows[1].error.as_deref().unwrap().contains("Add the port"));
        assert_eq!(
            rows[2].error.as_deref(),
            Some("This node is already listed above.")
        );
        assert_eq!(rows[3].error, None);
        assert_eq!(
            rows[1].address, "node.example.com",
            "what was typed is kept"
        );

        // Removing the bad row with its button leaves nothing to fix.
        let mut fields = form(&[
            ("node_mainnet_0_address", "node.example.com:18081"),
            ("node_mainnet_1_address", "node.example.com"),
        ]);
        fields.insert("node_action".into(), "remove:mainnet:1".into());
        assert!(!NodeForm::from_form(&fields, ORDER).has_errors());
    }

    #[test]
    fn rows_become_the_engines_json_and_back_with_every_field_in_order() {
        let rows = vec![
            NodeRow {
                address: "https://primary.example:18089".into(),
                ssl: false,
                self_signed: false,
                error: None,
            },
            NodeRow {
                address: "one.example:18081".into(),
                ssl: false,
                self_signed: true,
                error: None,
            },
            NodeRow {
                address: "[::1]:18081".into(),
                ssl: true,
                self_signed: true,
                error: None,
            },
            NodeRow {
                address: "three.example:18081".into(),
                ssl: true,
                self_signed: false,
                error: None,
            },
        ];
        let json = rows_to_setting(&rows).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "host": "primary.example", "port": 18089, "ssl": true, "accept_self_signed_certs": false,
                "fallbacks": [
                    { "host": "one.example", "port": 18081, "ssl": false, "accept_self_signed_certs": true, "fallbacks": [] },
                    { "host": "[::1]", "port": 18081, "ssl": true, "accept_self_signed_certs": true, "fallbacks": [] },
                    { "host": "three.example", "port": 18081, "ssl": true, "accept_self_signed_certs": false, "fallbacks": [] },
                ]
            })
        );
        let back = rows_from_setting(Some(&json));
        assert_eq!(
            back.iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            [
                "primary.example:18089",
                "one.example:18081",
                "[::1]:18081",
                "three.example:18081"
            ]
        );
        let back: Vec<NodeRow> = back.into_iter().map(|(_, row)| row).collect();
        assert_eq!(
            back[0],
            NodeRow {
                address: "primary.example:18089".into(),
                ssl: true,
                self_signed: false,
                error: None
            },
            "https:// became TLS"
        );
        assert_eq!(back[1..], rows[1..]);
        assert_eq!(rows_to_setting(&[]), None, "no rows clears the network");
    }

    #[test]
    fn a_saved_node_uses_the_engines_defaults_and_its_label() {
        let rows = rows_from_setting(Some(
            &serde_json::json!({ "host": "::1", "port": 18081, "fallbacks": [{ "host": "n", "port": 1, "fallbacks": [{ "host": "deep", "port": 2 }] }] }),
        ));
        assert_eq!(
            rows.len(),
            2,
            "a fallback's own fallbacks aren't used by the engine"
        );
        assert_eq!(rows[0].0, "::1:18081", "the engine's label, as saved");
        assert_eq!(
            rows[0].1,
            NodeRow {
                address: "[::1]:18081".into(),
                ssl: false,
                self_signed: true,
                error: None
            }
        );
        assert!(rows_from_setting(None).is_empty());
        assert!(rows_from_setting(Some(&serde_json::Value::Null)).is_empty());
    }

    #[test]
    fn only_node_form_fields_are_node_fields() {
        assert!(
            is_node_field("node_stagenet_0_address")
                && is_node_field("node_action")
                && is_node_field("node_mainnet_12_self_signed")
        );
        assert!(
            !is_node_field("node.stagenet")
                && !is_node_field("nodes_stagenet_0_address")
                && !is_node_field("node_Stagenet_0_address")
        );
    }
}
