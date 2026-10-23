//! What the two real-stagenet tests share: the public stagenet node they
//! use, and a check that it answers before anything is spent.

use std::time::Duration;

use engine::daemon::MoneroDaemonClient;

/// The fixed public stagenet node - the same one
/// `crates/e2e-harness/tests/support/mod.rs` uses.
pub mod node_fixture {
    pub const HOST: &str = "node.monerodevs.org";
    pub const PORT: u16 = 38089;
    pub const SSL: bool = false;
    pub const ACCEPT_SELF_SIGNED_CERTS: bool = true;
}

/// Confirms the stagenet node is reachable before doing anything else with
/// it: a misconfigured host/port or a down node would otherwise only surface
/// deep inside the scan-and-poll loop, after a real payment has been sent.
/// Retries a few times first: this public node occasionally drops a single
/// connection attempt even while it's up.
pub async fn require_daemon_reachable(daemon: &dyn MoneroDaemonClient, host: &str, port: u16) {
    if let Err(e) = retry(5, Duration::from_secs(3), || daemon.get_height()).await {
        panic!(
            "\n\ncannot reach the stagenet node at {host}:{port} (configured in node_fixture) after \
             several attempts: {e}\n\
             Check that host/port, your network connection, or try a different public stagenet \
             node (search \"monero stagenet public node\" for alternatives).\n"
        );
    }
}

/// Retries `f` up to `attempts` times, sleeping `delay` between tries, returning the
/// last error if every attempt fails - for the node's occasional transient
/// connection failures, not a genuinely unreachable node (which still fails
/// loudly after every attempt).
pub async fn retry<T, E, F, Fut>(attempts: u32, delay: Duration, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last_err = None;
    for attempt in 1..=attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("attempt {attempt}/{attempts} failed, retrying in {delay:?}: {e}");
                last_err = Some(e);
                if attempt < attempts {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.expect("attempts is always >= 1"))
}
