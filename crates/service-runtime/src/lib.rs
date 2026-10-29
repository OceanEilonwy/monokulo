//! What a long-running service process needs around its own work, the same
//! in the engine and monokulo: background loops that are restarted when
//! they panic ([`supervise`]), the signal to stop on and how long stopping
//! may take ([`shutdown`]), the machine the process runs on and its share
//! of it ([`resources`]), the clock in the units the services store
//! ([`time`]), and, for a standalone service, its status for this machine
//! alone ([`status`], feature `status`).

pub mod resources;
pub mod shutdown;
#[cfg(any(test, feature = "status"))]
pub mod status;
pub mod supervise;
pub mod time;
#[cfg(any(test, feature = "test-support"))]
pub mod unreachable;
