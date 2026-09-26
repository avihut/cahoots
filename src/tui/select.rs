//! The select prompt's state: which choice is highlighted, and what a key does
//! to it. No text and no terminal: `view` draws it, and `rail` runs it.

use super::keys::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Still being asked, with a choice highlighted.
    Open { highlighted: usize },
    /// Answered with a choice.
    Answered { chosen: usize },
    /// Left unanswered, on the choice it was highlighting.
    Left { highlighted: usize },
}

impl Select {
    /// Opens on the first choice, the one Enter alone takes.
    pub fn new() -> Select {
        Select::at(0)
    }

    /// Opens on this choice: the one a setting has now.
    pub fn at(highlighted: usize) -> Select {
        Select::Open { highlighted }
    }

    /// What `key` does among `count` choices. Up from the first is the last,
    /// and down from the last is the first, as in Clack, where ← and → move
    /// too. Once it is over, no key changes it.
    pub fn press(self, key: Key, count: usize) -> Select {
        let Select::Open { highlighted } = self else {
            return self;
        };
        match key {
            Key::Up | Key::Left => Select::Open {
                highlighted: (highlighted + count - 1) % count,
            },
            Key::Down | Key::Right => Select::Open {
                highlighted: (highlighted + 1) % count,
            },
            Key::Enter => Select::Answered {
                chosen: highlighted,
            },
            Key::Cancel | Key::Quit => Select::Left { highlighted },
            _ => self,
        }
    }

    /// The choice highlighted now, answered or not.
    pub fn highlighted(self) -> usize {
        match self {
            Select::Open { highlighted } | Select::Left { highlighted } => highlighted,
            Select::Answered { chosen } => chosen,
        }
    }

    pub fn is_over(self) -> bool {
        !matches!(self, Select::Open { .. })
    }

    /// The choice made, if one was.
    pub fn answer(self) -> Option<usize> {
        match self {
            Select::Answered { chosen } => Some(chosen),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(keys: &[Key]) -> Select {
        keys.iter()
            .fold(Select::new(), |state, key| state.press(*key, 3))
    }

    #[test]
    fn up_and_down_move_the_highlight_and_wrap_at_the_ends() {
        assert_eq!(after(&[]), Select::Open { highlighted: 0 });
        assert_eq!(
            after(&[Key::Down, Key::Down]),
            Select::Open { highlighted: 2 }
        );
        assert_eq!(
            after(&[Key::Down, Key::Down, Key::Down]),
            Select::Open { highlighted: 0 }
        );
        assert_eq!(after(&[Key::Up]), Select::Open { highlighted: 2 });
        assert_eq!(
            after(&[Key::Right, Key::Right, Key::Left]),
            Select::Open { highlighted: 1 },
            "← and → are ↑ and ↓"
        );
        assert_eq!(
            Select::at(2).press(Key::Down, 3),
            Select::Open { highlighted: 0 }
        );
        assert_eq!(
            after(&[Key::Down, Key::Other]),
            Select::Open { highlighted: 1 }
        );
    }

    #[test]
    fn enter_answers_esc_leaves_and_then_nothing_moves_it() {
        let answered = after(&[Key::Down, Key::Enter]);
        assert_eq!(answered, Select::Answered { chosen: 1 });
        assert_eq!(answered.answer(), Some(1));
        assert_eq!(answered.press(Key::Down, 3), answered);
        let left = after(&[Key::Down, Key::Cancel]);
        assert_eq!(left, Select::Left { highlighted: 1 });
        assert_eq!(after(&[Key::Quit]), Select::Left { highlighted: 0 });
        assert_eq!(left.answer(), None);
        assert!(left.is_over() && answered.is_over() && !Select::new().is_over());
    }
}
