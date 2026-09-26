//! The interface cahoots shows a person at a terminal: the Clack rail.
//! `docs/TUI.md` is the design and its rules.
//!
//! This is an interface and nothing more. It draws the text it is given, reads
//! keys, and answers with plain data (which choice, or none). It knows nothing
//! of meters, gates or runs: the command layer decides what to ask and what an
//! answer means (`src/cli/questions.rs`), and the logic never sees any of it
//! (hard rule 11). Inside, the layers are kept apart the same way:
//!
//! - `terminal` is the terminal itself: raw keys in, and everything given back.
//! - `keys` says what a terminal's bytes mean.
//! - `select` holds a prompt's state: which choice is highlighted, and when it
//!   is over.
//! - `view` says how things look: the rail's symbols and colors, and a prompt's
//!   frame for its state.
//! - `rail` holds a conversation: its intro, each question drawn and redrawn as
//!   keys come, and its outro.
//!
//! Only `terminal` touches a terminal, so the rest is data in, text out, and the
//! tests drive it with scripted keys. Everything is drawn on stderr, because
//! stdout carries the one JSON envelope.

mod keys;
mod rail;
mod select;
mod terminal;
mod view;

pub use keys::{Key, Keys};
pub use rail::Rail;
pub use terminal::Terminal;
pub use view::Colors;

/// One of the choices a question offers: its label, and a hint shown beside
/// it while it is highlighted (empty for none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub hint: String,
}

impl Choice {
    pub fn new(label: impl Into<String>, hint: impl Into<String>) -> Choice {
        Choice {
            label: label.into(),
            hint: hint.into(),
        }
    }
}
