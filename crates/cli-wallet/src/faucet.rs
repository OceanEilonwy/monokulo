//! Asking a public faucet to pay a wallet: `wallet-cli faucet`. Each
//! faucet is a plain HTTP request that answers with the payout's txid,
//! which the caller then records with [`crate::Wallet::add_output`].
//!
//! - `xmr-tw` (<https://stagenet-faucet.xmr-tw.org/>): stagenet only, a
//!   `GET /send_tx/?addr=` whose JSON `tx_id` is either the txid or a
//!   message saying why it wasn't sent.
//! - `cypherfaucet` (<https://cypherfaucet.com/api>): stagenet and testnet,
//!   0.01 XMR per address and per IP an hour, through its keyless API for
//!   CI and tooling (the web page's form wants a captcha; the API doesn't).

use std::time::Duration;

use monero_wallet::address::Network;
use serde::Deserialize;

use crate::{network_name, WalletError};

/// A faucet `wallet-cli faucet --provider` can ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Faucet {
    XmrTw,
    Cypherfaucet,
}

/// How long a faucet gets to answer: xmr-tw builds and sends the payout
/// before it replies.
const TIMEOUT: Duration = Duration::from_secs(180);

impl Faucet {
    pub const ALL: [Faucet; 2] = [Faucet::XmrTw, Faucet::Cypherfaucet];

    /// Its `--provider` name.
    pub fn name(self) -> &'static str {
        match self {
            Faucet::XmrTw => "xmr-tw",
            Faucet::Cypherfaucet => "cypherfaucet",
        }
    }

    /// Where its requests go; [`claim`] takes it as an argument so the
    /// tests can stand in for the faucet.
    pub fn url(self) -> &'static str {
        match self {
            Faucet::XmrTw => "https://stagenet-faucet.xmr-tw.org",
            Faucet::Cypherfaucet => "https://cypherfaucet.com",
        }
    }

    pub fn parse(name: &str) -> Result<Self, WalletError> {
        Faucet::ALL
            .into_iter()
            .find(|faucet| faucet.name() == name)
            .ok_or_else(|| {
                WalletError::Invalid(format!(
                    "{name} isn't a faucet this wallet knows - one of {:?}",
                    Faucet::ALL.map(Faucet::name)
                ))
            })
    }
}

/// What a faucet sent.
#[derive(Debug, PartialEq, Eq)]
pub struct Payout {
    pub txid: String,
    /// In XMR, as the faucet wrote it, when it says.
    pub amount: Option<String>,
}

/// Asks `faucet`, at `url` (normally [`Faucet::url`]), to pay `address` on
/// `network`.
pub async fn claim(
    faucet: Faucet,
    url: &str,
    network: Network,
    address: &str,
) -> Result<Payout, WalletError> {
    let url = url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("cli-wallet/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build reqwest client");
    let refused = |message: String| WalletError::Faucet {
        faucet: faucet.name(),
        message,
    };
    match faucet {
        Faucet::XmrTw => {
            if network != Network::Stagenet {
                return Err(refused(format!(
                    "it pays stagenet only, and this is a {} wallet",
                    network_name(network)
                )));
            }
            let response = client
                // Base58: nothing in an address needs escaping.
                .get(format!("{url}/send_tx/?addr={address}"))
                .send()
                .await
                .map_err(|e| refused(format!("request failed: {e}")))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|e| refused(format!("reading the answer failed: {e}")))?;
            #[derive(Deserialize)]
            struct Answer {
                tx_id: String,
            }
            let answer: Answer = serde_json::from_str(&body)
                .map_err(|_| refused(format!("HTTP {status}: {}", plain(&body))))?;
            match find_txid(&answer.tx_id) {
                Some(txid) => Ok(Payout { txid, amount: None }),
                None => Err(refused(plain(&answer.tx_id))),
            }
        }
        Faucet::Cypherfaucet => {
            let slug = match network {
                Network::Stagenet => "xmr-stagenet",
                Network::Testnet => "xmr-testnet",
                Network::Mainnet => return Err(refused("it pays no mainnet coins".into())),
            };
            let response = client
                .post(format!("{url}/api/v1/claim"))
                .json(&serde_json::json!({ "network": slug, "address": address }))
                .send()
                .await
                .map_err(|e| refused(format!("request failed: {e}")))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|e| refused(format!("reading the answer failed: {e}")))?;
            #[derive(Deserialize)]
            struct Answer {
                ok: bool,
                txid: Option<String>,
                amount: Option<String>,
                error: Option<String>,
                message: Option<String>,
                retry_after: Option<u64>,
            }
            let answer: Answer = serde_json::from_str(&body)
                .map_err(|_| refused(format!("HTTP {status}: {}", plain(&body))))?;
            match (answer.ok, answer.txid.as_deref().and_then(find_txid)) {
                (true, Some(txid)) => Ok(Payout {
                    txid,
                    amount: answer.amount,
                }),
                (true, None) => Err(refused(format!("HTTP {status}: no txid in {body}"))),
                (false, _) => {
                    let mut message = format!(
                        "{} ({})",
                        answer.message.as_deref().unwrap_or("refused"),
                        answer.error.as_deref().unwrap_or("no error code")
                    );
                    if let Some(seconds) = answer.retry_after {
                        message
                            .push_str(&format!("; try again in {} minutes", seconds.div_ceil(60)));
                    }
                    Err(refused(message))
                }
            }
        }
    }
}

/// The first 64-hex-digit word in `text`, lowercased.
fn find_txid(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .find(|word| word.len() == 64)
        .map(str::to_ascii_lowercase)
}

/// `text` without its HTML tags, on one line, cut short.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = out.split_whitespace().collect();
    let line = words.join(" ");
    match line.char_indices().nth(300) {
        Some((cut, _)) => format!("{}...", &line[..cut]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const ADDRESS: &str = "53etP59TCSWcnK3cJUyEdQAgTNCLWX92m39NJSHt3qnyVVYzqQEtXHnEF1Y7UkrfgwhndYnHmwxDu9RBAryjP2MHKWrENMo";
    const TXID: &str = "7103bd578034b6ff29a3d9bc0aa80ee121207aa0ad6e5f906ccba266a81081a5";

    /// A faucet that answers one request with `status` and `body`; the
    /// handle gives back the request it got.
    async fn faucet(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let length = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= head_end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            socket.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, handle)
    }

    #[tokio::test]
    async fn xmr_tw_is_asked_with_the_address_and_its_txid_is_read() {
        let body = format!(
            r#"{{"tx_id":"Sent! <a href=\"https://stagenet.xmrchain.net/tx/{TXID}\">{TXID}</a>"}}"#
        );
        let (url, request) = faucet("200 OK", &body).await;
        let payout = claim(Faucet::XmrTw, &url, Network::Stagenet, ADDRESS)
            .await
            .unwrap();
        assert_eq!(
            payout,
            Payout {
                txid: TXID.into(),
                amount: None
            }
        );
        let request = request.await.unwrap();
        assert!(
            request.starts_with(&format!("GET /send_tx/?addr={ADDRESS} ")),
            "{request}"
        );
    }

    #[tokio::test]
    async fn xmr_tw_says_why_it_sent_nothing() {
        // Word for word what it answers an address it won't pay.
        let body = r#"{"tx_id":"領取失敗：輸入內容錯誤或內部數值錯誤。<br>Failed: Input error or internal value error."}"#;
        let (url, _) = faucet("200 OK", body).await;
        let error = claim(Faucet::XmrTw, &url, Network::Stagenet, ADDRESS)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("the xmr-tw faucet sent nothing: ")
                && error.ends_with("Failed: Input error or internal value error."),
            "{error}"
        );
    }

    #[tokio::test]
    async fn xmr_tw_pays_stagenet_only() {
        let error = claim(
            Faucet::XmrTw,
            "http://127.0.0.1:1",
            Network::Testnet,
            ADDRESS,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("stagenet only, and this is a testnet wallet"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn cypherfaucet_is_asked_for_the_wallets_network_and_its_txid_is_read() {
        let body = format!(
            r#"{{"ok":true,"network":"xmr-testnet","amount":"0.01","txid":"{TXID}","tx_key":"ab"}}"#
        );
        let (url, request) = faucet("200 OK", &body).await;
        let payout = claim(Faucet::Cypherfaucet, &url, Network::Testnet, ADDRESS)
            .await
            .unwrap();
        assert_eq!(
            payout,
            Payout {
                txid: TXID.into(),
                amount: Some("0.01".into())
            }
        );
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /api/v1/claim "), "{request}");
        let body = &request[request.find("\r\n\r\n").unwrap() + 4..];
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            serde_json::json!({ "network": "xmr-testnet", "address": ADDRESS })
        );
    }

    #[tokio::test]
    async fn cypherfaucet_says_when_to_try_again() {
        // Word for word its answer to a second claim within the hour.
        let body = r#"{"ok":false,"error":"rate_limited","message":"You have already claimed within the current window.","retry_after":2297,"next_claim":"2026-10-10T12:10:27+00:00","source":"https://github.com/Tech1k/cypherfaucet.com"}"#;
        let (url, _) = faucet("429 Too Many Requests", body).await;
        let error = claim(Faucet::Cypherfaucet, &url, Network::Stagenet, ADDRESS)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "the cypherfaucet faucet sent nothing: You have already claimed within the current \
             window. (rate_limited); try again in 39 minutes"
        );
    }

    #[tokio::test]
    async fn a_page_instead_of_an_answer_is_shown_as_text() {
        let (url, _) = faucet(
            "503 Service Unavailable",
            "<html><body><h1>Down</h1> for upkeep</body></html>",
        )
        .await;
        let error = claim(Faucet::Cypherfaucet, &url, Network::Stagenet, ADDRESS)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "the cypherfaucet faucet sent nothing: HTTP 503 Service Unavailable: Down for upkeep"
        );
    }

    #[test]
    fn providers_are_named_as_the_flag_takes_them() {
        assert_eq!(Faucet::parse("xmr-tw").unwrap(), Faucet::XmrTw);
        assert_eq!(Faucet::parse("cypherfaucet").unwrap(), Faucet::Cypherfaucet);
        assert!(Faucet::parse("xmr_tw").is_err());
    }
}
