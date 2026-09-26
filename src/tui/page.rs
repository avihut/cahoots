//! The settings page's state: its rows, the one highlighted, the box opened
//! over the list to change one, and what the last change said. It draws
//! nothing (`view::page` does), and it knows nothing of settings: a row is
//! text and a way to change it, and a change goes out as an `Event` for the
//! command layer to carry out.

use super::Choice;
use super::keys::Key;
use super::order::{Order, Ordered};
use super::select::Select;
use super::stepper::{Stepped, Stepper, Unit};

/// One setting, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What names it, which stays the same when the rows are read again.
    pub id: String,
    pub section: String,
    pub label: String,
    pub value: String,
    pub origin: Origin,
    /// What it does.
    pub help: String,
    /// Where its value comes from, and anything else worth knowing now.
    pub note: String,
    pub edit: Edit,
}

/// Where a row's value comes from, which says how it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Nothing sets it: dim.
    Default,
    /// The person set it: plain, marked `•`.
    Set,
    /// Found on this machine: dim, as a default is.
    Found,
}

/// How a row is changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Enter turns it on or off, where it is.
    Toggle,
    /// One of these; `current` is marked ✓.
    Choose {
        choices: Vec<Choice>,
        current: Option<usize>,
    },
    /// A number, stepped with ← and →.
    Step {
        value: Option<i64>,
        min: i64,
        max: i64,
        unit: Unit,
        default: Option<i64>,
        /// Where stepping starts from nothing.
        start: i64,
    },
    /// These items, put in order.
    Order { items: Vec<String> },
    /// It cannot be changed now; the note says why.
    Fixed,
}

/// What the person answered for a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// A toggle, turned.
    Flip,
    /// This choice.
    Chose(usize),
    /// This number, or none (the default put back).
    Number(Option<i64>),
    /// The items in this order, by their places before.
    Order(Vec<usize>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Carry out this answer for the row at this place.
    Save(usize, Answer),
    /// The person is done with the page.
    Close,
}

/// The box open over the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Editor {
    Choose(Select),
    Step(Stepper),
    Order(Order),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Where the settings are kept, shown at the top.
    pub(super) file: String,
    pub(super) rows: Vec<Row>,
    pub(super) at: usize,
    /// The first line of the list in view.
    pub(super) scroll: usize,
    pub(super) editor: Option<Editor>,
    /// Why the last answer was not saved.
    pub(super) error: Option<String>,
    /// What the last change did.
    pub(super) flash: Option<String>,
}

impl Page {
    pub fn new(file: impl Into<String>, rows: Vec<Row>) -> Page {
        Page {
            file: file.into(),
            rows,
            at: 0,
            scroll: 0,
            editor: None,
            error: None,
            flash: None,
        }
    }

    /// What `key` does. Ctrl-C leaves the page from anywhere; Esc closes the
    /// box when one is open, and the page when none is.
    pub fn press(&mut self, key: Key) -> Option<Event> {
        if key == Key::Quit {
            return Some(Event::Close);
        }
        if self.rows.is_empty() {
            return matches!(key, Key::Cancel | Key::Char('q')).then_some(Event::Close);
        }
        if let Some(editor) = &mut self.editor {
            let answer = match editor {
                Editor::Choose(select) => {
                    let count = match &self.rows[self.at].edit {
                        Edit::Choose { choices, .. } => choices.len(),
                        _ => 0,
                    };
                    *select = select.press(key, count);
                    match *select {
                        Select::Open { .. } => return None,
                        Select::Answered { chosen } => Answer::Chose(chosen),
                        Select::Left { .. } => return self.close_box(),
                    }
                }
                Editor::Step(stepper) => match stepper.press(key) {
                    Stepped::Open => return None,
                    Stepped::Answered(value) => Answer::Number(value),
                    Stepped::Left => return self.close_box(),
                },
                Editor::Order(order) => match order.press(key) {
                    Ordered::Open => return None,
                    Ordered::Answered(items) => Answer::Order(items),
                    Ordered::Left => return self.close_box(),
                },
            };
            // Open again until the answer is saved or refused.
            if let Some(Editor::Choose(select)) = &mut self.editor {
                *select = Select::at(select.highlighted());
            }
            return Some(Event::Save(self.at, answer));
        }
        self.flash = None;
        self.error = None;
        let count = self.rows.len();
        match key {
            Key::Up => self.at = (self.at + count - 1) % count,
            Key::Down => self.at = (self.at + 1) % count,
            Key::Tab => self.jump(true),
            Key::BackTab => self.jump(false),
            Key::Enter | Key::Space => return self.open(),
            Key::Cancel | Key::Char('q') => return Some(Event::Close),
            _ => {}
        }
        None
    }

    fn close_box(&mut self) -> Option<Event> {
        self.editor = None;
        self.error = None;
        None
    }

    /// Opens the box for the highlighted row, or for a toggle, turns it.
    fn open(&mut self) -> Option<Event> {
        self.editor = match &self.rows[self.at].edit {
            Edit::Toggle => return Some(Event::Save(self.at, Answer::Flip)),
            Edit::Fixed => return None,
            Edit::Choose { current, .. } => Some(Editor::Choose(Select::at(current.unwrap_or(0)))),
            Edit::Step {
                value,
                min,
                max,
                unit,
                default,
                start,
            } => Some(Editor::Step(Stepper::new(
                *value,
                (*min, *max),
                *unit,
                *default,
                *start,
            ))),
            Edit::Order { items } => Some(Editor::Order(Order::new(items.len()))),
        };
        None
    }

    /// To the first row of the next section, or of the one before.
    fn jump(&mut self, forward: bool) {
        let starts: Vec<usize> = (0..self.rows.len())
            .filter(|&n| n == 0 || self.rows[n - 1].section != self.rows[n].section)
            .collect();
        let here = starts
            .iter()
            .rposition(|&start| start <= self.at)
            .unwrap_or(0);
        let to = if forward {
            (here + 1) % starts.len()
        } else if self.at == starts[here] {
            (here + starts.len() - 1) % starts.len()
        } else {
            here
        };
        self.at = starts[to];
    }

    /// The answer was saved: the rows as they are now, and what was done.
    /// The highlight stays on the same setting.
    pub fn saved(&mut self, rows: Vec<Row>, flash: impl Into<String>) {
        let id = self.rows.get(self.at).map(|row| row.id.clone());
        self.at = id
            .and_then(|id| rows.iter().position(|row| row.id == id))
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1));
        self.rows = rows;
        self.editor = None;
        self.error = None;
        self.flash = Some(flash.into());
    }

    /// The answer was refused, and why. The box stays open, to try again.
    pub fn refused(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.flash = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn row(id: &str, section: &str, edit: Edit) -> Row {
        Row {
            id: id.to_string(),
            section: section.to_string(),
            label: id.to_string(),
            value: String::new(),
            origin: Origin::Default,
            help: String::new(),
            note: String::new(),
            edit,
        }
    }

    fn page() -> Page {
        Page::new(
            "~/.config/cahoots/config.toml",
            vec![
                row("enabled", "Codex", Edit::Toggle),
                row(
                    "cap",
                    "Codex",
                    Edit::Step {
                        value: Some(75),
                        min: 1,
                        max: 100,
                        unit: Unit::Percent,
                        default: Some(75),
                        start: 75,
                    },
                ),
                row(
                    "meter",
                    "Usage meter",
                    Edit::Choose {
                        choices: vec![Choice::new("agent-usage", ""), Choice::new("ccusage", "")],
                        current: Some(1),
                    },
                ),
                row(
                    "advise",
                    "Roles",
                    Edit::Order {
                        items: vec!["codex".into(), "claude".into()],
                    },
                ),
                row("stop", "Roles", Edit::Fixed),
            ],
        )
    }

    fn press(page: &mut Page, keys: &[Key]) -> Vec<Event> {
        keys.iter().filter_map(|key| page.press(*key)).collect()
    }

    #[test]
    fn enter_turns_a_toggle_and_opens_a_box_for_the_rest() {
        let mut page = page();
        assert_eq!(
            press(&mut page, &[Key::Enter]),
            [Event::Save(0, Answer::Flip)]
        );
        assert!(page.editor.is_none());
        assert_eq!(
            press(
                &mut page,
                &[Key::Down, Key::Enter, Key::Left, Key::Left, Key::Enter]
            ),
            [Event::Save(1, Answer::Number(Some(65)))]
        );
        assert!(page.editor.is_some(), "open until it is saved");
        page.saved(page.rows.clone(), "Saved");
        assert!(page.editor.is_none());
        assert_eq!(page.flash.as_deref(), Some("Saved"));
    }

    #[test]
    fn a_choice_opens_on_what_is_chosen_now() {
        let mut page = page();
        page.at = 2;
        assert_eq!(
            press(&mut page, &[Key::Enter, Key::Enter]),
            [Event::Save(2, Answer::Chose(1))]
        );
        page.refused("no");
        assert_eq!(page.error.as_deref(), Some("no"));
        // Refused, the box is still open and can be answered again.
        assert_eq!(
            press(&mut page, &[Key::Up, Key::Enter]),
            [Event::Save(2, Answer::Chose(0))]
        );
    }

    #[test]
    fn a_list_is_reordered_in_its_box() {
        let mut page = page();
        page.at = 3;
        assert_eq!(
            press(&mut page, &[Key::Enter, Key::Space, Key::Down, Key::Enter]),
            [Event::Save(3, Answer::Order(vec![1, 0]))]
        );
    }

    #[test]
    fn esc_closes_the_box_then_the_page_and_ctrl_c_leaves_from_anywhere() {
        let mut page = page();
        page.at = 1;
        assert_eq!(press(&mut page, &[Key::Enter, Key::Right, Key::Cancel]), []);
        assert!(page.editor.is_none());
        assert_eq!(press(&mut page, &[Key::Cancel]), [Event::Close]);
        let mut page = self::page();
        page.at = 1;
        assert_eq!(press(&mut page, &[Key::Enter, Key::Quit]), [Event::Close]);
        // A row that can't be changed now opens nothing.
        let mut page = self::page();
        page.at = 4;
        assert_eq!(press(&mut page, &[Key::Enter]), []);
        assert!(page.editor.is_none());
    }

    #[test]
    fn tab_jumps_between_sections() {
        let mut page = page();
        page.press(Key::Tab);
        assert_eq!(page.at, 2);
        page.press(Key::Tab);
        assert_eq!(page.at, 3);
        page.press(Key::Tab);
        assert_eq!(page.at, 0, "and round again");
        page.press(Key::BackTab);
        assert_eq!(page.at, 3);
        page.press(Key::Down);
        page.press(Key::BackTab);
        assert_eq!(page.at, 3, "to the start of its own section first");
    }

    #[test]
    fn after_a_save_the_highlight_stays_on_its_setting() {
        let mut page = page();
        page.at = 3;
        let mut rows = page.rows.clone();
        rows.insert(0, row("new", "Codex", Edit::Toggle));
        page.saved(rows, "Saved");
        assert_eq!(page.rows[page.at].id, "advise");
    }
}
