//! The terminal a person is at, for as long as a question or the settings
//! page needs it: keys as they are pressed, nothing echoed, no cursor, and no
//! line wrap, so that each line of a frame is one row and a redraw lands where
//! it should. The page also takes the whole screen — the alternate screen,
//! which gives the shell's screen back as it was — learns its size by asking
//! the terminal, and hears when that size changes. All of it is given back
//! when the `Terminal` is dropped, and also by a panic hook, because a release
//! build aborts on a panic without unwinding.
//!
//! This is the one file of the interface that touches a terminal.

use std::io::{IsTerminal, Write};
use std::os::fd::AsFd;
#[cfg(not(target_os = "macos"))]
use std::os::fd::OwnedFd;
use std::panic::PanicHookInfo;
use std::sync::Arc;
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_os = "macos"))]
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::Signal;
#[cfg(not(target_os = "macos"))]
use nix::sys::signal::{SigSet, kill};
use nix::sys::termios::{self, InputFlags, LocalFlags, SetArg, SpecialCharacterIndices, Termios};
#[cfg(not(target_os = "macos"))]
use nix::unistd::{getpid, pipe};

use super::keys::{self, Key, Keys};

const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const NO_WRAP: &str = "\x1b[?7l";
const WRAP: &str = "\x1b[?7h";
/// The alternate screen, and back: the shell's screen is kept meanwhile.
const WHOLE_SCREEN: &str = "\x1b[?1049h\x1b[2J";
const SHELL_SCREEN: &str = "\x1b[?1049l";
/// The cursor to the far corner, where the terminal stops it, then "where is
/// it?": the answer is the screen's size.
const ASK_SIZE: &str = "\x1b[999;999H\x1b[6n";
/// How long a terminal gets to answer.
const ANSWER_WITHIN: Duration = Duration::from_millis(300);
/// A terminal that does not answer is taken to be this big.
const USUAL_SIZE: (u16, u16) = (80, 24);

type Hook = Arc<Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>>;

pub struct Terminal {
    /// The mode the terminal was in, to give back.
    saved: Termios,
    /// The panic hook there was before, to put back.
    hook: Option<Hook>,
    /// The page's whole screen, when this is the page's terminal.
    screen: Option<WholeScreen>,
}

impl Terminal {
    /// The terminal on stdin and stderr, set up for a question, or `None` when
    /// either one is not a terminal. (Whether it can move its cursor, which
    /// `TERM` says, is for the caller to know.)
    pub fn open() -> Option<Terminal> {
        Terminal::start(false)
    }

    /// The terminal set up for the settings page: as for a question, on the
    /// whole screen.
    pub fn full_screen() -> Option<Terminal> {
        Terminal::start(true)
    }

    /// The screen's columns and rows, as the terminal last said.
    pub fn size(&self) -> (u16, u16) {
        self.screen
            .as_ref()
            .map_or(USUAL_SIZE, |screen| screen.size)
    }

    fn start(whole: bool) -> Option<Terminal> {
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
            give_back(&Termios::from(mode), whole);
            before(info);
        }));
        write_err(&format!("{HIDE_CURSOR}{NO_WRAP}"));
        let screen = whole.then(|| {
            write_err(WHOLE_SCREEN);
            WholeScreen::start()
        });
        Some(Terminal {
            saved,
            hook: Some(hook),
            screen,
        })
    }
}

/// The terminal as it was: line by line, echoed, the cursor shown, lines
/// wrapping, and the shell's own screen.
fn give_back(saved: &Termios, whole: bool) {
    let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSADRAIN, saved);
    let shell = if whole { SHELL_SCREEN } else { "" };
    write_err(&format!("\x1b[0m{WRAP}{SHOW_CURSOR}{shell}"));
}

impl Drop for Terminal {
    fn drop(&mut self) {
        give_back(&self.saved, self.screen.is_some());
        if let Some(hook) = self.hook.take() {
            let _ = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
    }
}

impl Keys for Terminal {
    fn pressed(&mut self) -> Vec<Key> {
        match &mut self.screen {
            Some(screen) => screen.pressed(),
            None => keys_in(read_some()),
        }
    }
}

/// What the page's terminal has besides a question's: its size, and an ear
/// for the size changing.
struct WholeScreen {
    size: (u16, u16),
    /// Keys that came while the size was being asked.
    pending: Vec<u8>,
    resizes: Option<Resizes>,
}

impl WholeScreen {
    fn start() -> WholeScreen {
        let mut screen = WholeScreen {
            size: USUAL_SIZE,
            pending: Vec::new(),
            resizes: Resizes::start(),
        };
        screen.measure();
        screen
    }

    /// Asks the terminal its size, on stderr, and reads the answer on stdin,
    /// so a piped stdout changes nothing. With no answer in time, the size
    /// stays as it was.
    fn measure(&mut self) {
        write_err(ASK_SIZE);
        let deadline = Instant::now() + ANSWER_WITHIN;
        let mut got = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if !readable(left) {
                break;
            }
            let bytes = read_some();
            if bytes.is_empty() {
                break;
            }
            got.extend_from_slice(&bytes);
            let answer = (0..got.len()).find_map(|at| {
                keys::size_report(&got[at..]).map(|(len, rows, cols)| (at, len, rows, cols))
            });
            if let Some((at, len, rows, cols)) = answer {
                if rows > 0 && cols > 0 {
                    self.size = (cols, rows);
                }
                got.drain(at..at + len);
                break;
            }
        }
        self.pending.extend_from_slice(&got);
    }

    /// Keys, or word that the screen is a new size.
    fn pressed(&mut self) -> Vec<Key> {
        if !self.pending.is_empty() {
            return keys_in(std::mem::take(&mut self.pending));
        }
        loop {
            let (typed, resized) = {
                let stdin = std::io::stdin();
                let mut ready = vec![PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
                if let Some(resizes) = &self.resizes {
                    ready.push(PollFd::new(resizes.fd(), PollFlags::POLLIN));
                }
                match poll(&mut ready, PollTimeout::NONE) {
                    Ok(_) => {}
                    Err(Errno::EINTR) => continue,
                    Err(_) => return Vec::new(),
                }
                let fired = |fd: &PollFd<'_>| {
                    fd.revents()
                        .is_some_and(|events| !(events & !PollFlags::POLLNVAL).is_empty())
                };
                (fired(&ready[0]), ready.get(1).is_some_and(fired))
            };
            if resized {
                if !self.resizes.as_ref().is_some_and(Resizes::heard) {
                    // Readable with nothing heard: the ear is gone, and the
                    // screen keeps the size it has.
                    self.resizes = None;
                    continue;
                }
                self.measure();
                let mut keys = vec![Key::Resize(self.size.0, self.size.1)];
                keys.extend(keys_in(std::mem::take(&mut self.pending)));
                return keys;
            }
            if typed {
                // Nothing to read is the input ending.
                return keys_in(read_some());
            }
        }
    }
}

/// An ear for SIGWINCH, the signal a terminal sends when its size changes.
/// `pressed` polls it beside stdin. SIGWINCH is ignored unless a handler is
/// set, which takes unsafe code: macOS then drops it before any `sigwait`
/// sees it, but a kqueue still counts it; Linux keeps it pending while it is
/// held back, and a thread takes it with `sigwait`.
#[cfg(target_os = "macos")]
struct Resizes {
    queue: nix::sys::event::Kqueue,
}

#[cfg(target_os = "macos")]
impl Resizes {
    fn start() -> Option<Resizes> {
        use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
        let queue = Kqueue::new().ok()?;
        let winch = KEvent::new(
            Signal::SIGWINCH as usize,
            EventFilter::EVFILT_SIGNAL,
            EvFlags::EV_ADD,
            FilterFlag::empty(),
            0,
            0,
        );
        queue.kevent(&[winch], &mut [], Some(NOW)).ok()?;
        Some(Resizes { queue })
    }

    fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.queue.as_fd()
    }

    /// Takes what was heard: whether the size changed at all.
    fn heard(&self) -> bool {
        use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent};
        let empty = KEvent::new(
            0,
            EventFilter::EVFILT_SIGNAL,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        );
        let mut events = [empty; 4];
        matches!(self.queue.kevent(&[], &mut events, Some(NOW)), Ok(n) if n > 0)
    }
}

/// No wait at all, for a kqueue.
#[cfg(target_os = "macos")]
const NOW: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 0,
};

#[cfg(not(target_os = "macos"))]
struct Resizes {
    heard: OwnedFd,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The signals this thread held back before, to give back.
    mask: SigSet,
}

#[cfg(not(target_os = "macos"))]
impl Resizes {
    fn start() -> Option<Resizes> {
        let (heard, tell) = pipe().ok()?;
        let mut winch = SigSet::empty();
        winch.add(Signal::SIGWINCH);
        let mask = SigSet::thread_get_mask().ok()?;
        // Held back here before the thread starts, so the thread holds it
        // back too, and only its wait takes it.
        winch.thread_block().ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("resizes".to_string())
            .spawn(move || {
                while winch.wait().is_ok() && !stopped.load(Ordering::Relaxed) {
                    let _ = nix::unistd::write(&tell, &[1]);
                }
            });
        match thread {
            Ok(thread) => Some(Resizes {
                heard,
                stop,
                thread: Some(thread),
                mask,
            }),
            Err(_) => {
                let _ = mask.thread_set_mask();
                None
            }
        }
    }

    fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.heard.as_fd()
    }

    /// Takes what was heard: whether the size changed at all.
    fn heard(&self) -> bool {
        let mut heard = [0u8; 64];
        matches!(nix::unistd::read(self.heard.as_fd(), &mut heard), Ok(n) if n > 0)
    }
}

#[cfg(not(target_os = "macos"))]
impl Drop for Resizes {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the thread, which sees it is to stop.
        let _ = kill(getpid(), Signal::SIGWINCH);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.mask.thread_set_mask();
    }
}

/// The keys in `bytes`. A lone ESC is Esc, unless the rest of an arrow
/// follows at once.
fn keys_in(mut bytes: Vec<u8>) -> Vec<Key> {
    if bytes == [0x1b] && readable(Duration::from_millis(30)) {
        bytes.extend(read_some());
    }
    keys::decode(&bytes)
}

fn readable(within: Duration) -> bool {
    let stdin = std::io::stdin();
    let mut ready = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
    let ms = u16::try_from(within.as_millis()).unwrap_or(u16::MAX);
    matches!(poll(&mut ready, PollTimeout::from(ms)), Ok(n) if n > 0)
}

/// What is there to read on stdin, waiting for something. Nothing: the input
/// has ended.
fn read_some() -> Vec<u8> {
    let mut buf = [0u8; 256];
    loop {
        match nix::unistd::read(std::io::stdin().as_fd(), &mut buf) {
            Ok(read) => return buf[..read].to_vec(),
            Err(Errno::EINTR) => continue,
            Err(_) => return Vec::new(),
        }
    }
}

fn write_err(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
}
