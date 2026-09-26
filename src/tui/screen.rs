//! The page on a whole screen: drawn, and drawn again on every key and every
//! change of size, until a key makes an event for the command layer. Like
//! `rail`, it draws on what it is given and reads keys from what it is
//! given, so the tests drive it with scripted keys and read the screen.

use std::collections::VecDeque;
use std::io::Write;

use super::keys::{Key, Keys};
use super::page::{Event, Page};
use super::view::{Colors, page};

pub struct Screen<'a, W: Write> {
    out: &'a mut W,
    colors: Colors,
    /// Columns and rows.
    size: (u16, u16),
    /// Keys read after the one that made the last event.
    pending: VecDeque<Key>,
}

impl<'a, W: Write> Screen<'a, W> {
    pub fn new(out: &'a mut W, colors: Colors, size: (u16, u16)) -> Self {
        Screen {
            out,
            colors,
            size,
            pending: VecDeque::new(),
        }
    }

    /// Shows `page` and reads keys until one makes an event: an answer to
    /// save, or the person closing the page. Input that ends closes it.
    pub fn next(&mut self, page: &mut Page, keys: &mut impl Keys) -> Event {
        loop {
            self.draw(page);
            if self.pending.is_empty() {
                let pressed = keys.pressed();
                if pressed.is_empty() {
                    return Event::Close;
                }
                self.pending.extend(pressed);
            }
            while let Some(key) = self.pending.pop_front() {
                if let Key::Resize(cols, rows) = key {
                    self.size = (cols, rows);
                    continue;
                }
                if let Some(event) = page.press(key) {
                    return event;
                }
            }
        }
    }

    /// Draws the whole screen, each row from its start: nothing scrolls and
    /// nothing wraps.
    fn draw(&mut self, page: &mut Page) {
        let (cols, rows) = (usize::from(self.size.0), usize::from(self.size.1));
        let mut text = String::new();
        for (row, line) in page::render(page, cols, rows, self.colors)
            .iter()
            .enumerate()
        {
            text.push_str(&format!("\x1b[{};1H\x1b[2K{line}", row + 1));
        }
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::super::page::{Answer, Edit, Origin, Row};
    use super::*;

    fn page() -> Page {
        let row = |id: &str| Row {
            id: id.to_string(),
            section: "Codex".to_string(),
            label: id.to_string(),
            value: "off".to_string(),
            origin: Origin::Default,
            help: String::new(),
            note: String::new(),
            edit: Edit::Toggle,
        };
        Page::new("config.toml", vec![row("Enabled"), row("Writers in place")])
    }

    #[test]
    fn each_key_redraws_the_whole_screen_until_one_is_an_event() {
        let mut out = Vec::new();
        let mut page = page();
        let event = Screen::new(&mut out, Colors::OFF, (60, 10))
            .next(&mut page, &mut [Key::Down, Key::Enter].into_iter());
        assert_eq!(event, Event::Save(1, Answer::Flip));
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.matches("\x1b[1;1H\x1b[2K cahoots settings").count(), 2);
        assert!(
            out.ends_with("\x1b[10;1H\x1b[2K ↑↓ move · tab section · enter change · esc close"),
            "{out:?}"
        );
    }

    #[test]
    fn a_new_size_is_drawn_at_and_the_input_ending_closes() {
        let mut out = Vec::new();
        let mut page = page();
        let mut screen = Screen::new(&mut out, Colors::OFF, (40, 10));
        let event = screen.next(&mut page, &mut [Key::Resize(20, 6)].into_iter());
        assert_eq!(event, Event::Close);
        let out = String::from_utf8(out).unwrap();
        let last = &out[out.rfind("\x1b[2;1H").unwrap()..];
        assert!(
            last.starts_with(&format!("\x1b[2;1H\x1b[2K{}\x1b", "─".repeat(20))),
            "{last:?}"
        );
        assert!(
            out.contains("\x1b[6;1H")
                && !out[out.rfind("\x1b[1;1H").unwrap()..].contains("\x1b[7;1H")
        );
    }

    #[test]
    fn keys_after_an_event_wait_for_the_next_one() {
        let mut out = Vec::new();
        let mut page = page();
        let mut screen = Screen::new(&mut out, Colors::OFF, (40, 10));
        let mut keys =
            std::iter::once(vec![Key::Enter, Key::Down, Key::Enter]).chain(std::iter::empty());
        struct Batches<I>(I);
        impl<I: Iterator<Item = Vec<Key>>> Keys for Batches<I> {
            fn pressed(&mut self) -> Vec<Key> {
                self.0.next().unwrap_or_default()
            }
        }
        let mut keys = Batches(&mut keys);
        assert_eq!(
            screen.next(&mut page, &mut keys),
            Event::Save(0, Answer::Flip)
        );
        assert_eq!(
            screen.next(&mut page, &mut keys),
            Event::Save(1, Answer::Flip)
        );
        assert_eq!(screen.next(&mut page, &mut keys), Event::Close);
    }
}
