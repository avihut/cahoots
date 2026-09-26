//! What a terminal's bytes mean to a prompt or a page.

/// A key, as a prompt or a page understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Space,
    Tab,
    BackTab,
    /// A letter or a sign: `r` puts a number back to its default.
    Char(char),
    /// Esc: the person leaves what is open.
    Cancel,
    /// Ctrl-C or Ctrl-D, or the input ending: the person leaves altogether.
    Quit,
    /// The terminal is this many columns and rows now.
    Resize(u16, u16),
    Other,
}

/// Where keys come from: the terminal, or a test's script.
pub trait Keys {
    /// The keys pressed next, waiting for at least one. None at all means the
    /// input has ended.
    fn pressed(&mut self) -> Vec<Key>;
}

/// A script: its keys, one at a time.
impl<I: Iterator<Item = Key>> Keys for I {
    fn pressed(&mut self) -> Vec<Key> {
        self.next().into_iter().collect()
    }
}

/// The keys in what a terminal sent in one read. Arrows come as `ESC [ A`, or
/// `ESC O A` in application mode, and `k` `j` `h` `l` are ↑ ↓ ← →, as in Clack.
/// A lone ESC is Esc. With nothing echoed and no signals, Ctrl-C and Ctrl-D
/// arrive as bytes. The terminal's answer to a size question (`ESC [ r ; c R`)
/// is no key at all.
pub(super) fn decode(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let key = match bytes[at] {
            b'\r' | b'\n' => Some(Key::Enter),
            b' ' => Some(Key::Space),
            b'\t' => Some(Key::Tab),
            0x03 | 0x04 => Some(Key::Quit),
            b'k' => Some(Key::Up),
            b'j' => Some(Key::Down),
            b'h' => Some(Key::Left),
            b'l' => Some(Key::Right),
            0x1b if matches!(bytes.get(at + 1), Some(b'[' | b'O')) => {
                // Parameters, then one final byte: `ESC [ 1 ; 5 A` is Ctrl-↑.
                let mut end = at + 2;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                let key = match bytes.get(end) {
                    Some(b'A') => Some(Key::Up),
                    Some(b'B') => Some(Key::Down),
                    Some(b'C') => Some(Key::Right),
                    Some(b'D') => Some(Key::Left),
                    Some(b'Z') => Some(Key::BackTab),
                    Some(b'R') if size_report(&bytes[at..]).is_some() => None,
                    _ => Some(Key::Other),
                };
                at = end;
                key
            }
            0x1b => Some(Key::Cancel),
            byte if byte.is_ascii_graphic() => Some(Key::Char(byte as char)),
            _ => Some(Key::Other),
        };
        keys.extend(key);
        at += 1;
    }
    keys
}

/// The size the terminal's last answer in `bytes` says, as columns and rows:
/// an answer can come late, among keys.
pub(super) fn last_size(bytes: &[u8]) -> Option<(u16, u16)> {
    (0..bytes.len())
        .rev()
        .filter_map(|at| size_report(&bytes[at..]))
        .find(|&(_, rows, cols)| rows > 0 && cols > 0)
        .map(|(_, rows, cols)| (cols, rows))
}

/// The terminal's answer to "where is the cursor", `ESC [ rows ; cols R`, at
/// the start of `bytes`: its length, and the two numbers.
pub(super) fn size_report(bytes: &[u8]) -> Option<(usize, u16, u16)> {
    let rest = bytes.strip_prefix(b"\x1b[")?;
    let end = rest
        .iter()
        .position(|b| !(b.is_ascii_digit() || *b == b';'))?;
    if rest[end] != b'R' {
        return None;
    }
    let text = std::str::from_utf8(&rest[..end]).ok()?;
    let (rows, cols) = text.split_once(';')?;
    Some((end + 3, rows.parse().ok()?, cols.parse().ok()?))
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
        assert_eq!(decode(b"\x1b[D\x1b[C"), [Left, Right]);
        assert_eq!(
            decode(b"\x1b[1;5A"),
            [Up],
            "a modifier's parameters are read past"
        );
        assert_eq!(decode(b"khjl"), [Up, Left, Down, Right]);
    }

    #[test]
    fn enter_answers_esc_leaves_and_ctrl_c_and_ctrl_d_quit() {
        assert_eq!(decode(b"\r"), [Enter]);
        assert_eq!(decode(b"\n"), [Enter]);
        assert_eq!(decode(b"\x1b"), [Cancel]);
        assert_eq!(decode(b"\x03"), [Quit]);
        assert_eq!(decode(b"\x04"), [Quit]);
    }

    #[test]
    fn a_page_has_tab_space_and_letters_too() {
        assert_eq!(decode(b"\t\x1b[Z"), [Tab, BackTab]);
        assert_eq!(decode(b" r"), [Space, Char('r')]);
    }

    #[test]
    fn keys_that_mean_nothing_here_are_other_and_one_read_can_hold_several() {
        assert_eq!(decode(b"\x1b[H"), [Other], "Home");
        assert_eq!(decode(b"\x1bOR"), [Other], "F3, not a size");
        assert_eq!(decode(b"\x01"), [Other]);
        assert_eq!(decode(b"\x1b[B\x1b[B\r"), [Down, Down, Enter]);
        assert_eq!(decode(b""), []);
    }

    #[test]
    fn the_terminals_size_answer_is_no_key() {
        assert_eq!(size_report(b"\x1b[40;120R"), Some((9, 40, 120)));
        assert_eq!(size_report(b"\x1b[40;120Rx"), Some((9, 40, 120)));
        assert_eq!(size_report(b"\x1b[1;5A"), None);
        assert_eq!(decode(b"\x1b[B\x1b[40;120R\r"), [Down, Enter]);
    }

    #[test]
    fn a_late_size_answer_among_keys_still_says_the_size() {
        assert_eq!(last_size(b"\x1b[B\x1b[40;120R\r"), Some((120, 40)));
        assert_eq!(
            last_size(b"\x1b[24;80R\x1b[30;100R"),
            Some((100, 30)),
            "the last answer is the size now"
        );
        assert_eq!(last_size(b"\x1b[0;0R"), None, "no size at all");
        assert_eq!(last_size(b"\x1b[B\r"), None);
    }

    #[test]
    fn a_script_gives_its_keys_one_at_a_time_then_none() {
        let mut script = [Down, Enter].into_iter();
        assert_eq!(script.pressed(), [Down]);
        assert_eq!(script.pressed(), [Enter]);
        assert_eq!(script.pressed(), []);
    }
}
