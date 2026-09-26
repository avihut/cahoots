//! The interface cahoots shows a person at a terminal: questions on the
//! Clack rail, and the settings page on the whole screen. `docs/TUI.md` is
//! the design and its rules.
//!
//! This is an interface and nothing more. It draws the text it is given, reads
//! keys, and answers with plain data (which choice, which number, what order,
//! or none). It knows nothing of meters, gates, runs or settings: the command
//! layer decides what to ask and what an answer means (`src/cli/questions.rs`,
//! `src/cli/settings.rs`), and the logic never sees any of it (hard rule 11).
//! Inside, the layers are kept apart the same way:
//!
//! - `terminal` is the terminal itself: raw keys in, the whole screen and its
//!   size for the page, and everything given back.
//! - `keys` says what a terminal's bytes mean.
//! - `select`, `stepper` and `order` each hold one way of answering — a
//!   choice, a number, an order — and what a key does to it.
//! - `page` holds the settings page: its rows, the highlighted one, the box
//!   open to change one, and the events it sends out.
//! - `view` says how things look: the rail's symbols and colors and a
//!   prompt's frame; `view::canvas`, a grid to draw a whole screen on; and
//!   `view::page`, the page drawn on it.
//! - `rail` holds a conversation on the rail, and `screen` the page on the
//!   whole screen, each drawn and redrawn as keys come.
//!
//! Only `terminal` touches a terminal, so the rest is data in, text out, and the
//! tests drive it with scripted keys. Everything is drawn on stderr, because
//! stdout carries the one JSON envelope.

mod keys;
mod order;
mod page;
mod rail;
mod screen;
mod select;
mod stepper;
mod terminal;
mod view;

pub use keys::{Key, Keys};
pub use page::{Answer, Edit, Event, Origin, Page, Row};
pub use rail::Rail;
pub use screen::Screen;
pub use stepper::{Unit, number};
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
