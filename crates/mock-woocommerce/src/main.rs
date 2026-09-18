//! Thin CLI wrapper around [`mock_woocommerce::run_connect_flow`] - see
//! `docs/WOOCOMMERCE_WBS.md` 1.4.2. Drives the real WBS 1.4.1 connect flow
//! against a real, already-running monokulo instance and prints the
//! resulting credentials; exits `0` with valid credentials in hand, or
//! non-zero on any failure - the acceptance criterion the WBS spells out
//! directly ("run it against a live control plane in CI; exit 0 with valid
//! credentials in hand is the pass condition").
//!
//! No config-file/CLI-parsing system needed at this scale: the monokulo
//! URL is either the first CLI argument, the `MOCK_WOOCOMMERCE_CONTROL_PLANE_URL`
//! environment variable, or a hardcoded local default, checked in that order.

use std::process::ExitCode;

const DEFAULT_CONTROL_PLANE_BASE_URL: &str = "http://127.0.0.1:8081";

#[tokio::main]
async fn main() -> ExitCode {
    let monokulo_base_url = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("MOCK_WOOCOMMERCE_CONTROL_PLANE_URL").ok())
        .unwrap_or_else(|| DEFAULT_CONTROL_PLANE_BASE_URL.to_string());

    match mock_woocommerce::run_connect_flow(&monokulo_base_url).await {
        Ok(credentials) => {
            println!("connect flow succeeded against {monokulo_base_url}");
            println!("public_key={}", credentials.public_key);
            println!("endpoint={}", credentials.endpoint);
            println!("secret_token={}", credentials.secret_token);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("connect flow failed against {monokulo_base_url}: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiles() {
        assert!(true);
    }
}
