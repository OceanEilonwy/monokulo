//! The engine page (`docs/engine_visualizer.md`): what the engine's scanner
//! is doing, live, for admins.
//!
//! - [`machine`]: the page's logic, a pure state machine from the engine's
//!   activity events to the page's state, what to animate, and sentences.
pub mod machine;
