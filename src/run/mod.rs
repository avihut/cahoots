//! The run model. There is ONE path: every run is carried by a detached
//! supervisor, because a caller's tool call times out in minutes and a
//! delegated run does not. `run` is a thin client over the run directory,
//! which is the source of truth.

pub mod client;
pub mod record;
pub mod supervise;
