//! Log capture for tests that assert a failure was reported. One global
//! subscriber, installed on first use, sends each event to the buffer of the
//! thread that emitted it, if that thread is capturing. (A per-thread default
//! subscriber instead races with tracing's global callsite cache when tests
//! run side by side.) Events from other threads, such as the database
//! worker's, aren't captured.

use std::cell::RefCell;
use std::sync::{Arc, Once};

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

thread_local! {
    static SINK: RefCell<Option<Captured>> = const { RefCell::new(None) };
}

/// Writes to the current thread's capture, if any.
struct ThreadSink;

impl std::io::Write for ThreadSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| {
            if let Some(captured) = sink.borrow().as_ref() {
                captured.0.lock().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stops this thread's capture when dropped.
pub(crate) struct CaptureGuard(());

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        SINK.with(|sink| sink.borrow_mut().take());
    }
}

/// Captures every event this thread emits, at every level, until the guard
/// drops.
pub(crate) fn capture() -> (CaptureGuard, Captured) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| ThreadSink)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    let captured = Captured::default();
    SINK.with(|sink| *sink.borrow_mut() = Some(captured.clone()));
    (CaptureGuard(()), captured)
}
