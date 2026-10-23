//! The engine page's data: what the engine's
//! scanner is doing, live, for admins. The page itself is an app
//! (`apps/engine`) that draws and words what this sends.
//!
//! - [`machine`]: a pure state machine from the engine's activity events to
//!   the page's state, what to animate, and marks (what happened, as data).
//! - [`history`]: a network's events, with keyframes, for the state at any
//!   moment and the frames between two.
//! - [`relay`]: one engine stream per network, shared by every viewer.
pub mod history;
pub mod machine;
pub mod relay;

#[cfg(test)]
mod app_fixtures;
