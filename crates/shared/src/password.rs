//! Human-chosen account password hashing for the control plane.
//!
//! This is deliberately a *different* algorithm choice from
//! `shared::auth::RawToken::hash`: that module hashes a machine-generated,
//! high-entropy `sk_...` token with plain SHA-256, because a slow/memory-hard
//! hash buys no brute-force resistance for a token an attacker can't
//! meaningfully guess - it only adds cost. Human-chosen passwords are the
//! opposite case: low-entropy and guessable, so they genuinely need a
//! slow, memory-hard hash. Argon2id (via the `argon2` crate's own
//! recommended API) is that hash. Do not reuse this module for tokens, and
//! do not reuse `shared::auth` for passwords.

use std::sync::{Arc, LazyLock};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Password jobs allowed to run at once: one per CPU core. Each Argon2id
/// hash takes tens of milliseconds of CPU and about 19 MB of memory, so a
/// burst of sign-ins waits its turn here rather than spreading over the
/// blocking pool's hundreds of threads at once.
static SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    Arc::new(tokio::sync::Semaphore::new(cores))
});

/// The only way to hash or verify a password. [`run`] lends one to its
/// job, on the blocking pool; nothing outside this module can make one, and
/// a job only borrows it, so password work can't happen on an async thread
/// by mistake.
pub struct Hasher(());

/// Runs `f` on Tokio's blocking pool with the [`Hasher`], at most one job
/// per CPU core at a time, so password work never holds up an async worker
/// thread. `None` if the job panicked or the runtime is shutting down.
pub async fn run<T: Send + 'static>(f: impl FnOnce(&Hasher) -> T + Send + 'static) -> Option<T> {
    let slot = SLOTS.clone().acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        f(&Hasher(()))
    })
    .await
    .ok()
}

impl Hasher {
    /// Hashes `password` with Argon2id, using the crate's current
    /// recommended default parameters and a freshly random salt.
    ///
    /// Returns a self-describing PHC-format string (algorithm, parameters,
    /// salt, and hash all encoded together), so [`Hasher::verify`] needs no
    /// separate storage for salt or parameters.
    ///
    /// # Errors
    ///
    /// Returns an error only if the underlying hashing operation itself
    /// fails, which the `argon2` crate's API models as fallible but which
    /// should not happen in practice for valid input.
    pub fn hash(&self, password: &str) -> Result<String, argon2::password_hash::Error> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
        Ok(hash.to_string())
    }

    /// Verifies `password` against a previously stored PHC-format hash
    /// (as produced by [`Hasher::hash`]).
    ///
    /// Returns `false` both when the password is wrong and when `hashed` is
    /// not a valid PHC-format hash string at all - callers should not be
    /// able to distinguish "wrong password" from "malformed hash" from the
    /// return value alone, which rules out a boolean-shaped side channel.
    pub fn verify(&self, password: &str, hashed: &str) -> bool {
        let Ok(parsed_hash) = PasswordHash::new(hashed) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hashing through `run` leaves the async thread free: on a
    /// single-threaded runtime, another task keeps running meanwhile.
    #[tokio::test(flavor = "current_thread")]
    async fn run_hashes_without_holding_up_the_async_thread() {
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticker = {
            let ticks = ticks.clone();
            tokio::spawn(async move {
                loop {
                    ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::task::yield_now().await;
                }
            })
        };
        let hashed = run(|h| h.hash("correct horse battery staple"))
            .await
            .unwrap()
            .unwrap();
        ticker.abort();
        assert!(ticks.load(std::sync::atomic::Ordering::SeqCst) > 1);
        let verified = run(move |h| h.verify("correct horse battery staple", &hashed)).await;
        assert_eq!(verified, Some(true));
    }

    /// No more jobs run at once than there are slots.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_keeps_to_one_job_per_core() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slots = SLOTS.available_permits();
        let (running, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let jobs: Vec<_> = (0..slots * 3)
            .map(|_| {
                let (running, most) = (running.clone(), most.clone());
                tokio::spawn(run(move |_| {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    running.fetch_sub(1, Ordering::SeqCst);
                }))
            })
            .collect();
        for job in jobs {
            job.await.unwrap().unwrap();
        }
        assert!(most.load(Ordering::SeqCst) <= slots);
    }

    #[tokio::test]
    async fn a_panicking_job_answers_none() {
        let answer: Option<()> = run(|_| panic!("injected")).await;
        assert_eq!(answer, None);
    }

    #[test]
    fn a_hashed_password_verifies_against_the_original() {
        let hashed = Hasher(()).hash("correct horse battery staple").unwrap();
        assert!(Hasher(()).verify("correct horse battery staple", &hashed));
    }

    #[test]
    fn a_hashed_password_rejects_a_wrong_password() {
        let hashed = Hasher(()).hash("correct horse battery staple").unwrap();
        assert!(!Hasher(()).verify("wrong password", &hashed));
    }

    #[test]
    fn hashing_the_same_password_twice_produces_different_hashes_but_both_verify() {
        let password = "correct horse battery staple";
        let hash1 = Hasher(()).hash(password).unwrap();
        let hash2 = Hasher(()).hash(password).unwrap();

        // Per-call random salt, not fixed/reused.
        assert_ne!(hash1, hash2);

        assert!(Hasher(()).verify(password, &hash1));
        assert!(Hasher(()).verify(password, &hash2));
    }

    #[test]
    fn verifying_against_a_malformed_hash_string_returns_false_rather_than_panicking() {
        assert!(!Hasher(()).verify("whatever", "this is not a PHC-format hash"));
        assert!(!Hasher(()).verify("whatever", ""));
    }
}
