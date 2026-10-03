//! `RandomX` hashing for block verification, on a thread of its own
//! (`docs/proof_of_work.md`).
//!
//! `RandomX`'s objects can't leave the thread that made them, and hashing is
//! CPU work that mustn't run on a Tokio worker, so one thread per network
//! owns them and takes requests over a channel. Verification runs in light
//! mode: a 256 MiB cache per `RandomX` key, built once per key (about a
//! quarter of a second) and kept while in use; a key not used for a minute
//! is dropped when another is, so only around a key change are two held.
//! Dropping the [`Hasher`] ends the thread and frees its memory.
//!
//! The JIT compiler is used where it works (about 16 ms a hash, against
//! about 140 ms interpreted), with its pages never writable and executable
//! at once (`FLAG_SECURE`), as the programs it compiles come from blocks a
//! node sent.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};

/// A key unused for this long is dropped once another key is in use.
const IDLE_KEY: Duration = Duration::from_secs(60);
/// The most keys held at once (256 MiB each): the one in use and the one
/// before it, around a key change. Whoever names keys (a node, for an
/// anchor's window) can't make it hold more.
pub const MAX_KEYS: usize = 2;

/// What a hashing request failed with: `RandomX` couldn't be set up (out of
/// memory, say), or the thread is gone.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("RandomX: {0}")]
pub struct HashError(pub String);

/// What the hasher has done, for `/status`.
#[derive(Debug, Default)]
pub struct HasherStats {
    jit: AtomicBool,
    hashes: AtomicU64,
    hash_nanos: AtomicU64,
    keys_built: AtomicU64,
    key_build_nanos: AtomicU64,
    keys_held: AtomicU64,
}

/// A snapshot of [`HasherStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct HasherSnapshot {
    /// Whether `RandomX`'s JIT compiler is in use (otherwise interpreted).
    pub jit: bool,
    pub hashes: u64,
    /// Mean time a hash took, in milliseconds.
    pub mean_hash_ms: f64,
    pub keys_built: u64,
    /// Mean time building a key's cache took, in milliseconds.
    pub mean_key_build_ms: f64,
    /// Keys whose 256 MiB cache is held now.
    pub keys_held: u64,
}

impl HasherStats {
    pub fn snapshot(&self) -> HasherSnapshot {
        let mean = |total: u64, count: u64| {
            if count == 0 {
                0.0
            } else {
                total as f64 / count as f64 / 1e6
            }
        };
        let hashes = self.hashes.load(Ordering::Relaxed);
        let keys_built = self.keys_built.load(Ordering::Relaxed);
        HasherSnapshot {
            jit: self.jit.load(Ordering::Relaxed),
            hashes,
            mean_hash_ms: mean(self.hash_nanos.load(Ordering::Relaxed), hashes),
            keys_built,
            mean_key_build_ms: mean(self.key_build_nanos.load(Ordering::Relaxed), keys_built),
            keys_held: self.keys_held.load(Ordering::Relaxed),
        }
    }
}

type Reply = std::sync::mpsc::SyncSender<Result<Vec<[u8; 32]>, HashError>>;

struct Request {
    key: Vec<u8>,
    inputs: Vec<Vec<u8>>,
    reply: Reply,
}

/// A `RandomX` hashing thread. Cheap to share; the thread ends when the last
/// handle is dropped.
#[derive(Clone)]
pub struct Hasher {
    requests: std::sync::mpsc::Sender<Request>,
    stats: Arc<HasherStats>,
}

impl Hasher {
    /// Starts the thread. Nothing is allocated until the first request.
    pub fn start(name: &str) -> Result<Self, HashError> {
        let (requests, incoming) = std::sync::mpsc::channel::<Request>();
        let stats = Arc::new(HasherStats::default());
        let thread_stats = Arc::clone(&stats);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || serve(&incoming, &thread_stats))
            .map_err(|e| HashError(format!("can't start the hashing thread: {e}")))?;
        Ok(Self { requests, stats })
    }

    pub fn stats(&self) -> HasherSnapshot {
        self.stats.snapshot()
    }

    /// The `RandomX` hash of each of `inputs` under `key`, waiting on the
    /// calling thread: for blocking code (and building test chains).
    pub fn hash_blocking(
        &self,
        key: &[u8],
        inputs: Vec<Vec<u8>>,
    ) -> Result<Vec<[u8; 32]>, HashError> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.requests
            .send(Request {
                key: key.to_vec(),
                inputs,
                reply,
            })
            .map_err(|e| HashError(format!("the hashing thread has stopped: {e}")))?;
        answer
            .recv()
            .map_err(|e| HashError(format!("the hashing thread has stopped: {e}")))?
    }

    /// [`Self::hash_blocking`] from async code, waiting on the blocking
    /// pool rather than a Tokio worker.
    pub async fn hash(
        &self,
        key: [u8; 32],
        inputs: Vec<Vec<u8>>,
    ) -> Result<Vec<[u8; 32]>, HashError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.hash_blocking(&key, inputs))
            .await
            .map_err(|e| HashError(format!("the hashing task failed: {e}")))?
    }
}

/// One key's VM (which holds its cache), and when it was last used.
struct KeyedVm {
    key: Vec<u8>,
    vm: RandomXVM,
    used: Instant,
}

fn serve(incoming: &std::sync::mpsc::Receiver<Request>, stats: &HasherStats) {
    let mut keys: Vec<KeyedVm> = Vec::new();
    // Once the JIT has failed to set up, it isn't tried again.
    let mut jit_works = true;
    while let Ok(Request { key, inputs, reply }) = incoming.recv() {
        let now = Instant::now();
        keys.retain(|held| held.key == key || now.duration_since(held.used) < IDLE_KEY);
        let index = if let Some(index) = keys.iter().position(|held| held.key == key) {
            Ok(index)
        } else {
            // Room first, the least recently used key going.
            while keys.len() >= MAX_KEYS {
                let oldest = keys
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, held)| held.used)
                    .map_or(0, |(i, _)| i);
                keys.remove(oldest);
            }
            build(&key, &mut jit_works, stats).map(|vm| {
                keys.push(KeyedVm { key, vm, used: now });
                keys.len() - 1
            })
        };
        stats.keys_held.store(keys.len() as u64, Ordering::Relaxed);
        let result = index.and_then(|index| {
            let held = &mut keys[index];
            held.used = now;
            let mut out = Vec::with_capacity(inputs.len());
            for input in &inputs {
                let started = Instant::now();
                let hash = held
                    .vm
                    .calculate_hash(input)
                    .map_err(|e| HashError(e.to_string()))?;
                stats
                    .hash_nanos
                    .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                stats.hashes.fetch_add(1, Ordering::Relaxed);
                let hash: [u8; 32] = hash.try_into().map_err(|hash: Vec<u8>| {
                    HashError(format!("a hash of {} bytes, not 32", hash.len()))
                })?;
                out.push(hash);
            }
            Ok(out)
        });
        // The requester may have given up (its task was dropped).
        let _ = reply.send(result);
    }
}

/// A light-mode VM for `key`: with the JIT if it can be set up here, else
/// interpreted.
fn build(key: &[u8], jit_works: &mut bool, stats: &HasherStats) -> Result<RandomXVM, HashError> {
    let started = Instant::now();
    let recommended = RandomXFlag::get_recommended_flags();
    let attempts: Vec<RandomXFlag> = if *jit_works && recommended.contains(RandomXFlag::FLAG_JIT) {
        vec![
            recommended | RandomXFlag::FLAG_SECURE,
            recommended - RandomXFlag::FLAG_JIT,
        ]
    } else {
        vec![recommended - RandomXFlag::FLAG_JIT]
    };
    let mut last = None;
    for flags in attempts {
        let built = RandomXCache::new(flags, key)
            .and_then(|cache| RandomXVM::new(flags, Some(cache), None));
        match built {
            Ok(vm) => {
                let jit = flags.contains(RandomXFlag::FLAG_JIT);
                if !jit && *jit_works && recommended.contains(RandomXFlag::FLAG_JIT) {
                    *jit_works = false;
                    tracing::warn!(
                        error = ?last,
                        "RandomX's JIT compiler couldn't be set up here; verifying blocks interpreted, about nine times slower"
                    );
                }
                stats.jit.store(jit, Ordering::Relaxed);
                stats
                    .key_build_nanos
                    .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                stats.keys_built.fetch_add(1, Ordering::Relaxed);
                return Ok(vm);
            }
            Err(e) => last = Some(e.to_string()),
        }
    }
    Err(HashError(
        last.unwrap_or_else(|| "no flags to try".to_owned()),
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// `RandomX`'s own first test vector (tevador/RandomX `src/tests/tests.cpp`).
    #[test]
    fn hashes_randomx_s_reference_vector() {
        let hasher = Hasher::start("test randomx").unwrap();
        let digests = hasher
            .hash_blocking(b"test key 000", vec![b"This is a test".to_vec()])
            .unwrap();
        assert_eq!(
            hex::encode(digests[0]),
            "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f"
        );
        let stats = hasher.stats();
        assert_eq!((stats.hashes, stats.keys_built, stats.keys_held), (1, 1, 1));
    }

    /// However many keys are named, at most two caches are held.
    #[test]
    fn at_most_two_keys_are_held() {
        let hasher = Hasher::start("test randomx keys").unwrap();
        for key in [b"key one", b"key two", b"key thr", b"key one"] {
            hasher.hash_blocking(key, vec![b"x".to_vec()]).unwrap();
            assert!(hasher.stats().keys_held <= MAX_KEYS as u64);
        }
        assert_eq!(
            hasher.stats().keys_built,
            4,
            "the first was dropped and built again"
        );
    }
}
