//! What the engine and monokulo say to each other. Monokulo is the engine's
//! only caller: it asks over the engine's admin API and reads its `/status`
//! report, and both sides build and read those messages with the types
//! here, so a field or a status can't mean one thing to the engine and
//! another to monokulo.
//!
//! - [`ids`]: store, order and other identifiers as types of their own.
//! - [`network`]: Monero network names where they cross a text boundary.
//! - [`order_status`]: an order's status, as the engine decides it.
//! - [`activity`]: what the scanner is doing, event by event.
//! - [`announcements`], [`proof`] and [`scaling`]: the parts of `/status`
//!   about ZMQ announcements, proof-of-work checking and scaling.

pub mod activity;
pub mod announcements;
pub mod ids;
pub mod network;
pub mod order_status;
pub mod proof;
pub mod scaling;
