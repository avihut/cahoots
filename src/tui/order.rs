//! A list put in order without typing: ↑ and ↓ move the highlight; Space
//! picks the highlighted item up, ↑ and ↓ then move it, and Space puts it
//! down. No text and no terminal: `view` draws it.

use super::keys::Key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// The items as they stand now, by where each was at the start.
    pub items: Vec<usize>,
    pub at: usize,
    /// Whether the highlighted item is picked up.
    pub held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ordered {
    Open,
    /// Enter: the new order, as the items' places at the start.
    Answered(Vec<usize>),
    /// Esc: no answer.
    Left,
}

impl Order {
    pub fn new(count: usize) -> Order {
        Order {
            items: (0..count).collect(),
            at: 0,
            held: false,
        }
    }

    pub fn press(&mut self, key: Key) -> Ordered {
        let count = self.items.len();
        match key {
            Key::Up | Key::Down if count > 0 => {
                let last = count - 1;
                let to = match (key, self.held) {
                    // An item held stops at the ends; the highlight wraps.
                    (Key::Up, true) => self.at.saturating_sub(1),
                    (Key::Down, true) => (self.at + 1).min(last),
                    (Key::Up, false) => (self.at + last) % count,
                    _ => (self.at + 1) % count,
                };
                if self.held {
                    self.items.swap(self.at, to);
                }
                self.at = to;
                Ordered::Open
            }
            Key::Space => {
                self.held = !self.held;
                Ordered::Open
            }
            Key::Enter => Ordered::Answered(self.items.clone()),
            Key::Cancel | Key::Quit => Ordered::Left,
            _ => Ordered::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(keys: &[Key]) -> (Order, Ordered) {
        let mut order = Order::new(3);
        let mut last = Ordered::Open;
        for key in keys {
            last = order.press(*key);
        }
        (order, last)
    }

    #[test]
    fn the_highlight_moves_and_wraps_and_changes_nothing() {
        let (order, _) = after(&[Key::Up]);
        assert_eq!((order.at, order.items), (2, vec![0, 1, 2]));
    }

    #[test]
    fn a_held_item_moves_with_the_arrows_and_stops_at_the_ends() {
        let (order, last) = after(&[
            Key::Space,
            Key::Down,
            Key::Down,
            Key::Down,
            Key::Space,
            Key::Enter,
        ]);
        assert_eq!(order.items, [1, 2, 0], "the first is now last");
        assert_eq!(last, Ordered::Answered(vec![1, 2, 0]));
        let (order, _) = after(&[Key::Down, Key::Space, Key::Up, Key::Up]);
        assert_eq!(order.items, [1, 0, 2]);
        assert!(order.held);
    }

    #[test]
    fn esc_leaves_without_an_answer() {
        assert_eq!(
            after(&[Key::Space, Key::Down, Key::Cancel]).1,
            Ordered::Left
        );
    }
}
