//! How a command ends when a person reads it: what it did, what is worth
//! knowing, and its last word, on the rail like its questions, and then, off
//! the rail, the lines a person copies. This is data, the words already
//! chosen by the command layer. `view::ending` says how it looks, and
//! `Rail::end` draws it.

use super::page::Origin;

/// A command's end, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ending {
    /// The rail's title (`┌  title`), or `None` when a question the command
    /// asked has already opened the rail.
    pub title: Option<String>,
    pub blocks: Vec<Block>,
    pub last: Last,
    /// After the rail, flush left, so a block copies as it should be pasted.
    pub paste: Vec<Paste>,
}

impl Ending {
    /// An end with nothing on the rail but its title and its last word.
    pub fn just(title: Option<String>, last: Last) -> Ending {
        Ending {
            title,
            blocks: Vec::new(),
            last,
            paste: Vec::new(),
        }
    }
}

/// One stretch of the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A mark and its text, and lines under it.
    Said {
        mark: Mark,
        text: String,
        lines: Vec<String>,
    },
    /// Lines on the rail with no mark of their own.
    Lines(Vec<String>),
    /// `◇  title`, then a label and a value on each line, `•` where the
    /// value is set.
    Listing { title: String, items: Vec<Item> },
}

impl Block {
    /// `◇  text`: something done, and what it touched.
    pub fn done(text: impl Into<String>, lines: Vec<String>) -> Block {
        Block::Said {
            mark: Mark::Done,
            text: text.into(),
            lines,
        }
    }

    /// `●  text`: worth knowing.
    pub fn info(text: impl Into<String>, lines: Vec<String>) -> Block {
        Block::Said {
            mark: Mark::Info,
            text: text.into(),
            lines,
        }
    }

    /// `▲  text`: something left to do.
    pub fn warning(text: impl Into<String>, lines: Vec<String>) -> Block {
        Block::Said {
            mark: Mark::Warning,
            text: text.into(),
            lines,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// `◇`, green.
    Done,
    /// `●`, blue.
    Info,
    /// `▲`, yellow.
    Warning,
}

/// A value in a listing, and where it comes from: `•` when it is set,
/// dim when it is a default or was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub value: String,
    pub origin: Origin,
}

/// The rail's last line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Last {
    /// `└  message`: what was decided.
    Said(String),
    /// `└  message` in red: why nothing was.
    Refused(String),
}

/// Lines to copy, and where they go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paste {
    pub heading: String,
    pub lines: Vec<String>,
}
