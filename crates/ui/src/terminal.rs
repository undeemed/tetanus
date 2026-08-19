//! The terminal as a resource: taken for the duration of a view, and given
//! back on every path out of it.
//!
//! Everything else in this crate writes to a stream and leaves the terminal
//! as it found it. [`Screen`](crate::Screen) is the furthest that goes: it
//! moves the cursor up over its own block and no further, so a run that ends
//! mid-frame leaves a scrollback a person can still read. A full-screen view
//! cannot work that way. It needs keystrokes the moment they are pressed
//! rather than at the end of a line, and it needs a canvas that is not the
//! scrollback. Both are modes the terminal is put *into*, and a program that
//! exits without undoing them hands back a shell with no echo and no prompt.
//!
//! So this module has one job, and it is not drawing: hold those modes, and
//! be certain they are released. Drawing stays with `Ui` and `Screen`, which
//! already own the colour policy and the width. There is one rendering stack
//! in this binary, and this module does not start a second one.
//!
//! # Composition
//!
//! ```text
//! Console ── the trait: take the terminal, give it back
//! Keys    ── the trait: the next keystroke, or nothing within the wait
//! Tty     ── the real one: raw mode, alternate screen, hidden cursor
//! Held    ── the guard: takes on construction, restores on drop
//! Key     ── what a keystroke is, in this crate's own vocabulary
//! ```
//!
//! # Why a trait for two calls
//!
//! Because the interesting behaviour is *when* they are called, not what they
//! do, and the real one can only be exercised by a process that owns a
//! terminal. [`Console`] lets the lifecycle - taken once, restored once,
//! restored even when the body panicked - be asserted as plain data, the same
//! way [`Policy`](crate::Policy) lets colour be decided from plain data
//! instead of from a pty. `Tty` is then thin enough to read.
//!
//! # Panics
//!
//! A panic while the terminal is held unwinds, so [`Held`]'s `Drop` runs and
//! the terminal comes back. The panic *message* is a different matter: it is
//! written to the alternate screen a moment before that screen is left, so it
//! scrolls away with it. A view that wants the message kept catches the
//! unwind itself and reports after the guard is gone. This crate does not
//! install a panic hook, because a hook is global and a library that installs
//! one takes that decision away from the binary.
//!
//! # Signals
//!
//! [`Held`] covers every way out of a scope that Rust knows about. A signal is
//! not one of them: `SIGTERM`, `SIGHUP` and `SIGQUIT` end the process where it
//! stands, `Drop` never runs, and the person at the terminal keeps raw mode,
//! the alternate screen and a hidden cursor - a shell that echoes nothing,
//! drawn over a scrollback they cannot get back to. Their only way out is to
//! type `reset` blind.
//!
//! [`when_killed`] hangs that net, and it is hung by the binary rather than by
//! `Held`, for the reason the panic hook is not: a signal handler is process
//! wide, and taking a terminal in a test should not quietly register one. It
//! restores and then re-raises with the default handler, so what the signal
//! means does not change - a killed process still reports itself killed, by
//! that signal, to whatever is waiting on it.
//!
//! # Why this crate's own [`Key`]
//!
//! For the reason [`Role`](crate::Role) exists rather than an `anstyle::Style`
//! in every signature: the vocabulary a view codes against is this crate's,
//! and which crate reads the bytes underneath is an implementation detail this
//! one is free to change. It also keeps the mapping - which is where the
//! judgement is, and where the sharp edges are - a pure function over values a
//! test can write down.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, terminal};

/// What a full-screen view needs from the terminal, and nothing more.
pub trait Console {
    /// Put the terminal into the state the view needs.
    fn take(&mut self) -> io::Result<()>;

    /// Put it back the way it was.
    ///
    /// Called on the way out of every view, including the ones that are
    /// unwinding, so an implementation attempts every step even after one of
    /// them fails.
    fn restore(&mut self) -> io::Result<()>;
}

/// Where a view's keystrokes come from.
///
/// Separate from [`Console`] because they are separate powers: taking the
/// terminal is what a view must undo, reading it is what a view is driven by,
/// and the test double for a loop needs to script the second without
/// pretending to do the first. [`Tty`] answers both, so a real view names
/// `Console + Keys` and gets one object.
pub trait Keys {
    /// The next keystroke, or `None` if `wait` passed without one.
    ///
    /// Returning on a timeout is what lets a view repaint a spinner while
    /// nobody is typing. Anything that is not a keystroke this crate names -
    /// a mouse report, a focus change, the release half of a key - reads as
    /// `None` as well, so a caller that treats `None` as "nothing happened"
    /// is correct in every case.
    fn key(&mut self, wait: Duration) -> io::Result<Option<Key>>;
}

/// The real terminal, over the stream a view draws on.
///
/// Raw mode is a property of the process's controlling terminal rather than
/// of the stream, so it is switched by a call that takes no argument. The
/// alternate screen and the cursor are escape codes, and those go to the
/// stream the caller named - which is what lets a view draw on stderr and
/// leave stdout for a machine, as `tetanus serve` already does.
pub struct Tty<W: Write> {
    out: W,
}

impl<W: Write> Tty<W> {
    /// Wrap the stream the view will draw on.
    pub fn new(out: W) -> Self {
        Self { out }
    }
}

impl<W: Write> Keys for Tty<W> {
    fn key(&mut self, wait: Duration) -> io::Result<Option<Key>> {
        if !event::poll(wait)? {
            return Ok(None);
        }
        Ok(key_of(event::read()?))
    }
}

impl<W: Write> Console for Tty<W> {
    fn take(&mut self) -> io::Result<()> {
        // Raw mode first: if the alternate screen fails after it, `restore`
        // still undoes both, whereas a failure between them in the other
        // order would leave a screen nobody is holding.
        terminal::enable_raw_mode()?;
        execute!(self.out, terminal::EnterAlternateScreen, cursor::Hide)
    }

    fn restore(&mut self) -> io::Result<()> {
        // Both steps, always, and the first failure reported. Skipping the
        // rest after one fails is how a user ends up on their own scrollback
        // with no echo: the worst of the two halves rather than either.
        let screen = execute!(self.out, cursor::Show, terminal::LeaveAlternateScreen);
        let raw = terminal::disable_raw_mode();
        screen.and(raw)
    }
}

/// The terminal in raw mode, and nothing else.
///
/// [`Tty`] takes three things at once: raw mode, the alternate screen and the
/// cursor. A prompt wants one of the three. It is drawn on the reader's own
/// scrollback, where the conversation above it has to stay, and the cursor is
/// the whole point of a line being edited.
///
/// It holds no stream because it writes none: raw mode is a property of the
/// process's controlling terminal, and everything this mode is taken for is
/// drawn by the caller on the stream it already has.
#[derive(Debug, Clone, Copy)]
pub struct Typing;

impl Console for Typing {
    fn take(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()
    }

    fn restore(&mut self) -> io::Result<()> {
        terminal::disable_raw_mode()
    }
}

impl Keys for Typing {
    fn key(&mut self, wait: Duration) -> io::Result<Option<Key>> {
        if !event::poll(wait)? {
            return Ok(None);
        }
        Ok(key_of(event::read()?))
    }
}

/// The terminal, held.
///
/// Construction takes it and dropping gives it back, so the give-back cannot
/// be forgotten and cannot be skipped by an early `return`, a `?`, or a panic.
/// It is the bargain `tetanus-core`'s effect handles already make, applied to
/// a resource that is not ours: the terminal belongs to the person at it, and
/// a view is only borrowing it.
pub struct Held<C: Console> {
    console: C,
    /// `false` once the terminal has been given back, so it is never given
    /// back twice - `release()` followed by the drop at the end of the scope
    /// is the ordinary way that would otherwise happen.
    holding: bool,
}

impl<C: Console> Held<C> {
    /// Take the terminal.
    ///
    /// A failure returns the console rather than swallowing it, because the
    /// caller that could not enter the view still has a stream to report on.
    pub fn take(mut console: C) -> Result<Self, (C, io::Error)> {
        match console.take() {
            Ok(()) => Ok(Self {
                console,
                holding: true,
            }),
            // Nothing was taken, so there is nothing to restore. Calling
            // `restore` here on the off chance would undo a state the process
            // was already in when it started.
            Err(err) => Err((console, err)),
        }
    }

    /// Give the terminal back now, and say whether it worked.
    ///
    /// The drop does the same thing with no way to report, which is right for
    /// an unwind and wrong for an ordinary exit: a view that ends normally
    /// with a terminal it could not restore has something to tell the user,
    /// on the very stream that is now in an unknown state.
    pub fn release(mut self) -> io::Result<()> {
        self.give_back()
    }

    /// The console, for a view that has to draw through it.
    pub fn console(&mut self) -> &mut C {
        &mut self.console
    }

    fn give_back(&mut self) -> io::Result<()> {
        if !self.holding {
            return Ok(());
        }
        self.holding = false;
        self.console.restore()
    }
}

impl<C: Console> Drop for Held<C> {
    fn drop(&mut self) {
        // The error has nowhere to go: this runs during an unwind as often as
        // not, and a panic here would abort the process over a terminal that
        // is already misbehaving. `release` is the way to hear about it.
        let _ = self.give_back();
    }
}

/// Give the terminal back when the process is killed rather than ended.
///
/// A watch on the four signals that end a process politely enough to be
/// caught: `SIGTERM` from a supervisor or a `kill`, `SIGHUP` from a terminal
/// that closed, `SIGQUIT` and `SIGINT` from a keyboard whose keys this view is
/// not reading. When one lands, `spare` is restored and the signal is
/// re-raised with the default handler, so the process still dies of what
/// killed it.
///
/// `spare` is a second console over the same terminal, because the first one
/// is inside a [`Held`] the running view is using and the watch runs on a
/// thread of its own. For [`Tty`] that costs a second handle on one stream.
///
/// Arm it just before the terminal is taken rather than just after. Undoing a
/// mode nothing has entered is what a terminal ignores, so the earlier order
/// costs nothing, and the later one leaves a gap in which the screen has been
/// entered and no watch will leave it.
///
/// The returned guard takes the watch down when it is dropped. Bind it: a
/// `let _ = when_killed(..)` drops it on the spot and watches nothing.
///
/// # Errors
///
/// If the handlers cannot be registered. A caller that only wanted the net
/// should carry on without it rather than refuse to open the view: the failure
/// leaves the terminal exactly as safe as it was before this function existed.
#[cfg(unix)]
pub fn when_killed<C: Console + Send + 'static>(mut spare: C) -> io::Result<Killed> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGTERM, SIGHUP, SIGQUIT, SIGINT])?;
    let handle = signals.handle();
    let thread = std::thread::spawn(move || {
        // `forever` ends when the handle is closed, which is the guard being
        // dropped and nothing having arrived. Only a signal takes this arm.
        if let Some(signal) = signals.forever().next() {
            // Whatever it says goes nowhere: the process is about to end, and
            // the stream that would carry the complaint is the one in doubt.
            let _ = spare.restore();
            // Not `exit`: a caller waiting on this process asked what killed
            // it, and an exit status would tell them nobody did.
            let _ = signal_hook::low_level::emulate_default_handler(signal);
        }
    });
    Ok(Killed {
        handle: Some(handle),
        thread: Some(thread),
    })
}

/// [`when_killed`] where there are no signals to watch.
#[cfg(not(unix))]
pub fn when_killed<C: Console + Send + 'static>(_spare: C) -> io::Result<Killed> {
    Ok(Killed {})
}

/// The watch [`when_killed`] hung, taken down when this is dropped.
#[cfg(unix)]
pub struct Killed {
    /// `None` once taken down, which is `Drop` and nothing else.
    handle: Option<signal_hook::iterator::Handle>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for Killed {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.close();
        }
        // Joined rather than left running, so that a view which ends and then
        // reports on the same stream cannot be writing at the same time as a
        // watch that is on its way out.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The watch [`when_killed`] hung, taken down when this is dropped.
#[cfg(not(unix))]
pub struct Killed {}

/// A keystroke, in the vocabulary a view codes against.
///
/// Non-exhaustive because a later slice adds paste and the function keys, and
/// a view that matched exhaustively today would be the thing that stopped
/// compiling for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Key {
    /// A character that was typed, modifiers already applied by the terminal.
    Char(char),
    Enter,
    Tab,
    /// Shift-Tab, which terminals report as its own key rather than as a
    /// modified `Tab`.
    BackTab,
    Backspace,
    Delete,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// A character held with Control. Lower-cased, because a terminal reports
    /// Ctrl-C and Ctrl-Shift-C alike and a view that matched `'C'` would miss
    /// half of them.
    Ctrl(char),
    /// A character held with Alt, reported by terminals as an escape prefix.
    Alt(char),
    /// The window changed size, in columns and rows.
    ///
    /// It arrives on the same queue as the keys because it has to be handled
    /// in the same place: a frame drawn for the old width is wrong the instant
    /// this is read, and a view that polled the size separately would draw at
    /// least one frame that was.
    Resize(u16, u16),
}

/// One event, as this crate names it - or `None` for one it does not.
///
/// `Ctrl` and `Alt` are separate variants rather than a modifier set on
/// `Char`, because that is how they are used: a view matches `Ctrl('c')`, and
/// never asks "was this an `a` with something held". Shift is not among them
/// for the opposite reason - the terminal has already applied it, and the
/// character that arrives is the shifted one.
fn key_of(event: Event) -> Option<Key> {
    let key = match event {
        Event::Key(key) => key,
        Event::Resize(cols, rows) => return Some(Key::Resize(cols, rows)),
        // A mouse report, a focus change, a paste: real events that this
        // vocabulary does not name yet. Dropping them is right; guessing at
        // one is not.
        _ => return None,
    };
    // A terminal in the enhanced keyboard protocol reports the release of
    // every key as well as its press, and Windows does so always. A view that
    // took both would act on every keystroke twice - one of those bugs that
    // shows up on one person's terminal and on nobody else's.
    if key.kind == KeyEventKind::Release {
        return None;
    }
    Some(named(key))
}

fn named(key: KeyEvent) -> Key {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) if alt => Key::Alt(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Esc => Key::Esc,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        // A function key, a media key, a keypad that reported itself as one.
        // `Esc` is the honest answer for a key with no name here: it is the
        // byte the terminal would have sent for most of them anyway, and it
        // is the one key every view already handles.
        _ => Key::Esc,
    }
}

/// Test Design Specification: holding the terminal, and naming a keystroke.
///
/// Features tested: that the terminal is taken once and given back exactly
/// once, on the ordinary path, the early-return path, and the unwinding path;
/// that a failed take leaves nothing to give back; and the whole mapping from
/// a terminal event to this crate's [`Key`], including the two events that
/// must produce nothing.
///
/// Features NOT tested here: [`Tty`] itself. Its two methods are calls into
/// `crossterm` against a real controlling terminal, which a `cargo test`
/// process does not have. What is testable about it - the order of the steps,
/// and that a failed step does not skip the rest - is stated in its own doc
/// and reviewed there.
///
/// Environmental needs: none. No case opens a terminal.
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// A console that records what was asked of it, and can be told to fail.
    #[derive(Default)]
    struct Fake {
        log: Rc<RefCell<Vec<&'static str>>>,
        take_fails: bool,
    }

    impl Fake {
        fn new() -> (Self, Rc<RefCell<Vec<&'static str>>>) {
            let log = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    log: Rc::clone(&log),
                    take_fails: false,
                },
                log,
            )
        }
    }

    impl Console for Fake {
        fn take(&mut self) -> io::Result<()> {
            if self.take_fails {
                return Err(io::Error::other("no terminal"));
            }
            self.log.borrow_mut().push("take");
            Ok(())
        }

        fn restore(&mut self) -> io::Result<()> {
            self.log.borrow_mut().push("restore");
            Ok(())
        }
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// TC-UI-TERM-1: a guard that is simply dropped.
    /// Expected: `["take", "restore"]`. The give-back is not something a
    /// caller opts into; it is what the end of the scope means.
    #[test]
    fn dropping_the_guard_gives_the_terminal_back() {
        let (fake, log) = Fake::new();
        {
            let _held = Held::take(fake).ok().expect("taken");
            assert_eq!(*log.borrow(), ["take"]);
        }
        assert_eq!(*log.borrow(), ["take", "restore"]);
    }

    /// TC-UI-TERM-2: `release()`, then the drop at the end of the scope.
    /// Expected: one `restore`, and `Ok(())` from the release. Restoring
    /// twice is not harmless - the second `LeaveAlternateScreen` pops a
    /// screen the view never pushed, taking the user's scrollback with it.
    #[test]
    fn releasing_then_dropping_restores_once() {
        let (fake, log) = Fake::new();
        let held = Held::take(fake).ok().expect("taken");
        held.release().expect("released");
        assert_eq!(*log.borrow(), ["take", "restore"]);
    }

    /// TC-UI-TERM-3: the terminal could not be taken.
    /// Expected: no `restore`, and the console handed back with the error.
    /// A restore here would undo a state the process was already in, and the
    /// caller still needs the stream to say why the view did not open.
    #[test]
    fn a_failed_take_leaves_nothing_to_restore() {
        let (mut fake, log) = Fake::new();
        fake.take_fails = true;
        let (_console, err) = Held::take(fake).err().expect("refused");
        assert_eq!(err.to_string(), "no terminal");
        assert!(log.borrow().is_empty());
    }

    /// TC-UI-TERM-4: the view panics while the terminal is held.
    /// Expected: `restore` still ran. This is the case the guard exists for -
    /// a panic with the terminal in raw mode leaves a shell with no echo, and
    /// the user's next move is to type `reset` blind.
    #[test]
    fn a_panic_still_gives_the_terminal_back() {
        let (fake, log) = Fake::new();
        let watch = Rc::clone(&log);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = Held::take(fake).ok().expect("taken");
            panic!("the view fell over");
        }));
        assert!(panicked.is_err());
        assert_eq!(*watch.borrow(), ["take", "restore"]);
    }

    /// TC-UI-TERM-5: every event this crate names.
    /// Expected: the pairs below, exactly. The three worth reading twice are
    /// Ctrl-Shift-C folding onto `Ctrl('c')`, a shifted letter arriving as
    /// the shifted character with no modifier variant, and a resize being a
    /// key like any other.
    #[test]
    fn an_event_is_named_the_way_a_view_matches_it() {
        let cases = [
            (press(KeyCode::Char('a')), Key::Char('a')),
            (press(KeyCode::Enter), Key::Enter),
            (press(KeyCode::Tab), Key::Tab),
            (press(KeyCode::BackTab), Key::BackTab),
            (press(KeyCode::Backspace), Key::Backspace),
            (press(KeyCode::Delete), Key::Delete),
            (press(KeyCode::Esc), Key::Esc),
            (press(KeyCode::Left), Key::Left),
            (press(KeyCode::Right), Key::Right),
            (press(KeyCode::Up), Key::Up),
            (press(KeyCode::Down), Key::Down),
            (press(KeyCode::Home), Key::Home),
            (press(KeyCode::End), Key::End),
            (press(KeyCode::PageUp), Key::PageUp),
            (press(KeyCode::PageDown), Key::PageDown),
            (Event::Resize(80, 24), Key::Resize(80, 24)),
            (
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                Key::Ctrl('c'),
            ),
            (
                Event::Key(KeyEvent::new(
                    KeyCode::Char('C'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                )),
                Key::Ctrl('c'),
            ),
            (
                Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
                Key::Alt('b'),
            ),
            (
                Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
                Key::Char('A'),
            ),
            // No name here yet, and `Esc` is what the terminal would have
            // sent for it before the enhanced protocol existed.
            (press(KeyCode::F(5)), Key::Esc),
        ];

        for (event, want) in cases {
            assert_eq!(key_of(event), Some(want), "{event:?}");
        }
    }

    /// TC-UI-TERM-6: the two events that must produce nothing.
    /// Expected: `None` for both. A key release is the enhanced keyboard
    /// protocol's second half, and taking it would act on every keystroke
    /// twice on the terminals that send it and once everywhere else. A focus
    /// change is simply not a keystroke.
    #[test]
    fn a_release_and_a_non_key_are_not_keystrokes() {
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(key_of(release), None);
        assert_eq!(key_of(Event::FocusGained), None);
    }

    /// TC-UI-TERM-7: the repeat half of a held key.
    /// Expected: `Key::Char('a')`. A repeat is a real keystroke - it is what
    /// holding Backspace to delete a word is made of - so it is kept, which
    /// is the decision `KeyEventKind::Release` alone does not state.
    #[test]
    fn a_repeat_is_a_keystroke() {
        let repeat = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        assert_eq!(key_of(repeat), Some(Key::Char('a')));
    }
}
