//! The terminal a person is at, for as long as a question needs it: keys as
//! they are pressed, nothing echoed, no cursor, and no line wrap, so that each
//! line of a frame is one row and a redraw lands on the one before. All of it
//! is given back when the `Terminal` is dropped, and also by a panic hook,
//! because a release build aborts on a panic without unwinding.
//!
//! This is the one file of the interface that touches a terminal.

use std::io::{IsTerminal, Write};
use std::os::fd::AsFd;
use std::panic::PanicHookInfo;
use std::sync::Arc;

use nix::errno::Errno;
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{self, InputFlags, LocalFlags, SetArg, SpecialCharacterIndices, Termios};

use super::keys::{self, Key, Keys};

const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const NO_WRAP: &str = "\x1b[?7l";
const WRAP: &str = "\x1b[?7h";

type Hook = Arc<Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>>;

pub struct Terminal {
    /// The mode the terminal was in, to give back.
    saved: Termios,
    /// The panic hook there was before, to put back.
    hook: Option<Hook>,
}

impl Terminal {
    /// The terminal on stdin and stderr, set up for a question, or `None` when
    /// either one is not a terminal. (Whether it can move its cursor, which
    /// `TERM` says, is for the caller to know.)
    pub fn open() -> Option<Terminal> {
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            return None;
        }
        let stdin = std::io::stdin();
        let saved = termios::tcgetattr(stdin.as_fd()).ok()?;
        let mut keys_only = saved.clone();
        keys_only
            .local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG | LocalFlags::IEXTEN);
        keys_only
            .input_flags
            .remove(InputFlags::IXON | InputFlags::ICRNL);
        keys_only.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        keys_only.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        // Flushed: keys typed before the question was asked don't answer it.
        termios::tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, &keys_only).ok()?;

        let hook: Hook = Arc::new(std::panic::take_hook());
        let before = Arc::clone(&hook);
        let mode = libc::termios::from(saved.clone());
        std::panic::set_hook(Box::new(move |info| {
            give_back(&Termios::from(mode));
            before(info);
        }));
        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "{HIDE_CURSOR}{NO_WRAP}");
        let _ = stderr.flush();
        Some(Terminal {
            saved,
            hook: Some(hook),
        })
    }
}

/// The terminal as it was: line by line, echoed, the cursor shown, lines
/// wrapping.
fn give_back(saved: &Termios) {
    let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSADRAIN, saved);
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "{WRAP}{SHOW_CURSOR}");
    let _ = stderr.flush();
}

impl Drop for Terminal {
    fn drop(&mut self) {
        give_back(&self.saved);
        if let Some(hook) = self.hook.take() {
            let _ = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
    }
}

impl Keys for Terminal {
    fn pressed(&mut self) -> Vec<Key> {
        let stdin = std::io::stdin();
        let fd = stdin.as_fd();
        let mut buf = [0u8; 64];
        let read = loop {
            match nix::unistd::read(fd, &mut buf) {
                Err(Errno::EINTR) => continue,
                Ok(read) => break read,
                Err(_) => break 0,
            }
        };
        let mut bytes = buf[..read].to_vec();
        // A lone ESC is Esc, unless the rest of an arrow follows at once.
        if bytes == [0x1b] {
            let mut ready = [PollFd::new(fd, PollFlags::POLLIN)];
            if matches!(poll(&mut ready, PollTimeout::from(30u16)), Ok(n) if n > 0)
                && let Ok(more) = nix::unistd::read(fd, &mut buf)
            {
                bytes.extend_from_slice(&buf[..more]);
            }
        }
        keys::decode(&bytes)
    }
}
