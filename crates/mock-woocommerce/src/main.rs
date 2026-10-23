//! Thin CLI wrapper around [`mock_woocommerce::run_connect_flow_without_webhook`]:
//! runs the connect flow against an already-running monokulo and prints the
//! resulting credentials. Exits `0` with valid credentials in hand, or
//! non-zero on any failure.
//!
//! The monokulo URL is the first CLI argument, the `MOCK_WOOCOMMERCE_MONOKULO_URL`
//! environment variable, or a local default, checked in that order.

use std::process::ExitCode;

const DEFAULT_MONOKULO_URL: &str = "http://127.0.0.1:8081";

#[tokio::main]
async fn main() -> ExitCode {
    let monokulo_base_url = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("MOCK_WOOCOMMERCE_MONOKULO_URL").ok())
        .unwrap_or_else(|| DEFAULT_MONOKULO_URL.to_string());

    // No webhook: this process exits once it has printed the credentials, so
    // a receiver it ran would leave the store with a dead webhook URL.
    match mock_woocommerce::run_connect_flow_without_webhook(&monokulo_base_url).await {
        Ok(credentials) => {
            println!("connect flow succeeded against {monokulo_base_url}");
            println!("public_key={}", credentials.public_key);
            println!("api={}", credentials.api_url());
            println!("secret_token={}", credentials.secret_token);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("connect flow failed against {monokulo_base_url}: {err}");
            ExitCode::FAILURE
        }
    }
}
