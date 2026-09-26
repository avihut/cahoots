//! What a terminal's bytes mean to a prompt.

/// A key, as a prompt understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    /// Esc, Ctrl-C or Ctrl-D: the person leaves the question.
    Cancel,
    Other,
}

/// Where a prompt's keys come from: the terminal, or a test's script.
pub trait Keys {
    /// The keys pressed next, waiting for at least one. None at all means the
    /// input has ended, and the question is left unanswered.
    fn pressed(&mut self) -> Vec<Key>;
}

/// A script: its keys, one at a time.
impl<I: Iterator<Item = Key>> Keys for I {
    fn pressed(&mut self) -> Vec<Key> {
        self.next().into_iter().collect()
    }
}

/// The keys in what a terminal sent in one read. Arrows come as `ESC [ A`, or
/// `ESC O A` in application mode; ← and → move too, and `k`/`h` and `j`/`l`, as
/// in Clack. A lone ESC is Esc. With nothing echoed and no signals, Ctrl-C and
/// Ctrl-D arrive as bytes, and leave the question like Esc.
pub(super) fn decode(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let key = match bytes[at] {
            b'\r' | b'\n' => Key::Enter,
            0x03 | 0x04 => Key::Cancel,
            b'k' | b'h' => Key::Up,
            b'j' | b'l' => Key::Down,
            0x1b if matches!(bytes.get(at + 1), Some(b'[' | b'O')) => {
                // Parameters, then one final byte: `ESC [ 1 ; 5 A` is Ctrl-↑.
                let mut end = at + 2;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                let key = match bytes.get(end) {
                    Some(b'A' | b'D') => Key::Up,
                    Some(b'B' | b'C') => Key::Down,
                    _ => Key::Other,
                };
                at = end;
                key
            }
            0x1b => Key::Cancel,
            _ => Key::Other,
        };
        keys.push(key);
        at += 1;
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::Key::*;
    use super::*;

    #[test]
    fn arrows_move_in_either_mode_and_so_do_clacks_letters() {
        assert_eq!(decode(b"\x1b[A"), [Up]);
        assert_eq!(decode(b"\x1b[B"), [Down]);
        assert_eq!(decode(b"\x1bOA"), [Up], "application cursor mode");
        assert_eq!(decode(b"\x1bOB"), [Down]);
        assert_eq!(decode(b"\x1b[D\x1b[C"), [Up, Down], "← and →");
        assert_eq!(
            decode(b"\x1b[1;5A"),
            [Up],
            "a modifier's parameters are read past"
        );
        assert_eq!(decode(b"khjl"), [Up, Up, Down, Down]);
    }

    #[test]
    fn enter_answers_and_esc_ctrl_c_and_ctrl_d_leave() {
        assert_eq!(decode(b"\r"), [Enter]);
        assert_eq!(decode(b"\n"), [Enter]);
        assert_eq!(decode(b"\x1b"), [Cancel]);
        assert_eq!(decode(b"\x03"), [Cancel]);
        assert_eq!(decode(b"\x04"), [Cancel]);
    }

    #[test]
    fn keys_that_mean_nothing_here_are_other_and_one_read_can_hold_several() {
        assert_eq!(decode(b"\x1b[H"), [Other], "Home");
        assert_eq!(decode(b"x1 "), [Other, Other, Other]);
        assert_eq!(decode(b"\x1b[B\x1b[B\r"), [Down, Down, Enter]);
        assert_eq!(decode(b""), []);
    }

    #[test]
    fn a_script_gives_its_keys_one_at_a_time_then_none() {
        let mut script = [Down, Enter].into_iter();
        assert_eq!(script.pressed(), [Down]);
        assert_eq!(script.pressed(), [Enter]);
        assert_eq!(script.pressed(), []);
    }
}
