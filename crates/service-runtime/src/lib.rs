//! What a long-running service process needs around its own work, the same
//! in the engine and monokulo: background loops that are restarted when
//! they panic ([`supervise`]), the signal to stop on and how long stopping
//! may take ([`shutdown`]), the machine the process runs on and its share
//! of it ([`resources`]), and the clock in the units the services store
//! ([`time`]).

pub mod resources;
pub mod shutdown;
pub mod supervise;
pub mod time;
#[cfg(any(test, feature = "test-support"))]
pub mod unreachable;
