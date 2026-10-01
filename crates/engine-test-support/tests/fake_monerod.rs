//! The stand-in monerod (`fake-monerod`) answers what the engine asks of a
//! node with nothing to scan: the engine's own client against the binary
//! the end-to-end suites run. If the client starts asking a node something
//! new, this is where the stand-in is found not to know it.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};

use engine::daemon::{ChainTip, MoneroDaemonClient};
use engine::daemon_rpc::RpcDaemonClient;

/// A running `fake-monerod`, stopped when dropped.
struct Fake {
    child: Child,
    /// Kept open: the process writes its one line here.
    _stdout: BufReader<ChildStdout>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(height: u64) -> (Fake, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fake-monerod"))
        .args(["--height", &height.to_string()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    // Owned by the guard before anything can fail, so a missing or
    // malformed readiness line kills the child instead of leaking it.
    let mut fake = Fake {
        child,
        _stdout: stdout,
    };
    let mut line = String::new();
    fake._stdout.read_line(&mut line).unwrap();
    let port = line
        .trim()
        .strip_prefix("FAKE_MONEROD_READY ")
        .and_then(|address| address.rsplit(':').next())
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("no address from fake-monerod: {line:?}"));
    (fake, port)
}

#[tokio::test]
async fn the_stand_in_node_answers_what_an_idle_engine_asks() {
    let (_fake, port) = start(50);
    let client = RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap();

    // The tip, with its id.
    let tip = client.get_tip().await.unwrap();
    assert_eq!(tip.height, 50);
    let tip_id = tip.hash.clone().expect("the tip's id with its height");
    assert_eq!(client.get_height().await.unwrap(), 50);
    assert_eq!(client.get_info().await.unwrap().height, Some(50));

    // A block's hash alone, and not one past the tip.
    assert_eq!(client.get_block_hash(50).await.unwrap(), tip_id);
    assert!(client.get_block_hash(51).await.is_err());

    // Headers, for blocks nobody is scanned for: a range, and a range
    // running past the tip (what there is of it).
    let headers = client.get_chain_headers(48, 3).await.unwrap();
    assert_eq!(
        headers.iter().map(|h| h.height).collect::<Vec<_>>(),
        vec![48, 49, 50]
    );
    assert_eq!(headers[2].hash, tip_id);
    assert_eq!(headers[2].prev_hash, headers[1].hash);
    let at_tip = client.get_chain_headers(50, 5).await.unwrap();
    assert_eq!(at_tip.len(), 1);
    assert_eq!(at_tip[0], headers[2]);

    // The pool: it can't say its changes (no `get_blocks.bin`), so its
    // plain, empty list is read; asked with the tip, the same.
    assert!(client.get_mempool_txids().await.unwrap().is_empty());
    let (tip_again, pool) = client.get_tip_and_mempool().await;
    assert_eq!(
        tip_again.unwrap(),
        ChainTip {
            height: 50,
            hash: Some(tip_id)
        }
    );
    assert!(pool.unwrap().is_empty());
}
