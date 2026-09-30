//! Log capture for tests that assert a failure was reported. The capture is
//! this thread's default subscriber, so it sees what the test's own
//! (current-thread) runtime logs and nothing from tests running alongside.

use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Clone, Default)]
pub(crate) struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }

    /// How many captured lines contain `needle`.
    pub(crate) fn count(&self, needle: &str) -> usize {
        self.text().lines().filter(|line| line.contains(needle)).count()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Captures every event on this thread, at every level, until the guard
/// drops.
pub(crate) fn capture() -> (tracing::subscriber::DefaultGuard, Captured) {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (tracing::subscriber::set_default(subscriber), captured)
}
