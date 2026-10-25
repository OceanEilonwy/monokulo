//! The engine page's data: what the engine's scanner is doing, live, for
//! admins. The page itself is an app (`apps/engine`) that draws and words
//! what monokulo sends; this crate is what it is sent. It does no I/O, so
//! the page's logic is stepped through and tested on its own.
//!
//! - [`machine`]: a pure state machine from the engine's activity events to
//!   the page's state, what to animate, and marks (what happened, as data).
//! - [`history`]: a network's events, with keyframes, for the state at any
//!   moment and the frames between two.
//!
//! Monokulo's relay (`monokulo::engine_relay`) keeps one engine stream per
//! network, shared by every viewer, and feeds it into a [`history::History`].
pub mod history;
pub mod machine;

#[cfg(test)]
mod app_fixtures;
