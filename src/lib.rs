//! cahoots — lets coding-agent harnesses delegate to each other.
//!
//! The binary is a thin shell over this library so that every module is
//! reachable from tests. `AGENTS.md` holds the hard rules; `docs/` the design.

#![forbid(unsafe_code)]

pub mod cli;
pub mod config;
pub mod dirs;
pub mod env;
pub mod exit;
pub mod harness;
pub mod model;
pub mod registry;
