//! cahoots — lets coding-agent harnesses delegate to each other.
//!
//! The binary is a thin shell over this library so that every module is
//! reachable from tests. `AGENTS.md` holds the hard rules; `docs/` the design.

#![forbid(unsafe_code)]

pub mod calibrate;
pub mod cli;
pub mod config;
pub mod dirs;
pub mod doctor;
pub mod env;
pub mod exit;
pub mod gate;
pub mod harness;
pub mod history;
pub mod install;
pub mod learn;
pub mod model;
pub mod paths;
pub mod pick;
pub mod placement;
pub mod registry;
pub mod report;
pub mod review;
pub mod run;
pub mod spawn;
