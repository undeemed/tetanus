//! The loop a full-screen view runs in.
//!
//! [`Held`] answers "the terminal comes back" and [`Frame`] answers "the
//! screen is painted in one pass". Between them is the part every view
//! repeats: take the terminal, paint, wait for a keystroke, decide whether to
//! carry on, and give the terminal back on the way out however the way out
//! was reached. Written once per view, that is where a view forgets to handle
//! a resize, or traps a user whose Ctrl-C it never checked for.
//!
//! So it is written once here, and a view supplies only the two things that
//! are actually its own: what the next frame looks like, and what a keystroke
//! means.
//!
//! # Composition
//!
//! ```text
//! View  ── the trait a view implements: frame(), key(), tick()
//! Flow  ── what a view says back: go round again, or stop
//! Show  ── the loop's own settings: the first size, and how long to wait
//! show  ── the driver: takes the terminal, runs the loop, gives it back
//! Stop  ── why the loop ended, which is what decides the exit status
//! ```
//!
//! # Two keystrokes the view never sees
//!
//! **Ctrl-C stops the loop**, before the view is asked. A view is a program
//! inside a program, and the one thing a user knows for certain about a
//! terminal is that Ctrl-C gets them out of it. Leaving that to each view
//! means one view eventually forgets, and the person is left with a screen
//! they cannot leave and a terminal they will have to `reset`. It ends the
//! loop as [`Stop::Interrupted`], which contract §4.5 exits `130`.
//!
//! **A resize is swallowed**, after the loop has noted the new size. A view
//! learns its size the only way it can use one - as the arguments to
//! [`View::frame`], the moment before it builds a frame at that size - so
//! there is nothing for a view to do with the event itself, and a view that
//! ignored it would silently keep drawing at the old size.
//!
//! Everything else reaches [`View::key`] as it arrived, `q` included: which
//! key quits is the view's own vocabulary, not this module's.
//!
//! # Why the frame is rebuilt rather than repainted
//!
//! The loop asks for a whole new frame after every event, and never keeps the
//! last one. That is [`Frame`]'s bargain taken one level up: at the sizes a
//! terminal has, building the rows costs less than deciding which of them
//! changed, and a view with no retained frame has no way to disagree with the
//! screen. A view that finds this too expensive caches inside itself, where it
//! knows what its own content did; the loop stays honest.

use std::io::{self, Write};
use std::time::Duration;

use crate::frame::Frame;
use crate::terminal::{Console, Held, Key, Keys};
use crate::writer::Ui;

/// What a full-screen view supplies to the loop.
pub trait View {
    /// The next frame, built for the size the terminal has now.
    ///
    /// Called once before the first keystroke and once after every event, so
    /// a view that has nothing new to say still returns a frame - the same
    /// one it would have built before.
    fn frame(&mut self, cols: usize, rows: usize) -> Frame;

    /// Answer a keystroke.
    ///
    /// Ctrl-C and a resize never arrive here; see the module documentation.
    fn key(&mut self, key: Key) -> Flow;

    /// Answer a wait that passed with nothing typed.
    ///
    /// This is where a view that is watching something else - a turn arriving,
    /// a spinner turning - takes its next look. A view with nothing to watch
    /// keeps the default and is repainted, unchanged, once per wait.
    fn tick(&mut self) -> Flow {
        Flow::Go
    }
}

/// What a view says back to the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Paint again and keep waiting.
    Go,
    /// Leave the view.
    Stop,
}

/// Why the loop ended.
///
/// Returned rather than folded into `Ok(())` because the two ends are two exit
/// statuses: a view the user left is a success, and one they interrupted is
/// `130` under contract §4.5. A caller that could not tell them apart would
/// have to guess, and would guess `0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The view asked to stop.
    Quit,
    /// Ctrl-C.
    Interrupted,
}

/// The loop's own settings.
pub struct Show {
    /// The size the first frame is built for. Every later frame follows the
    /// terminal, because a resize reports the new size with the event.
    pub size: (usize, usize),
    /// How long to wait for a keystroke before calling [`View::tick`].
    pub wait: Duration,
}

impl Default for Show {
    fn default() -> Self {
        Self {
            size: size(),
            // Ten times a second: below what a person reads as lag, and far
            // above what a terminal is worth repainting more often than.
            wait: Duration::from_millis(100),
        }
    }
}

/// The terminal's size in columns and rows.
///
/// Falls back to the size a terminal is assumed to have when there is none to
/// ask - a redirected stream, a CI job - so a view is buildable off a
/// terminal even though it is only usable on one.
pub fn size() -> (usize, usize) {
    terminal_size::terminal_size()
        .map(|(cols, rows)| (cols.0 as usize, rows.0 as usize))
        .unwrap_or((80, 24))
}

/// Take the terminal, run `view` in it, and give the terminal back.
///
/// The give-back happens on every path out, the failing ones included, and its
/// own failure is reported only when the loop had none of its own: a loop that
/// failed has the more useful thing to say, and the caller can only report one.
pub fn show<C, W, V>(console: C, ui: &mut Ui<W>, view: &mut V, show: Show) -> io::Result<Stop>
where
    C: Console + Keys,
    W: Write,
    V: View,
{
    let mut held = Held::take(console).map_err(|(_, err)| err)?;
    let stopped = drive(&mut held, ui, view, show);
    let given = held.release();
    let stopped = stopped?;
    given?;
    Ok(stopped)
}

/// The loop, with the terminal already held.
///
/// Split out so that every `?` in it unwinds into `show`'s give-back rather
/// than out of the function that is holding the terminal.
fn drive<C, W, V>(
    held: &mut Held<C>,
    ui: &mut Ui<W>,
    view: &mut V,
    Show { size, wait }: Show,
) -> io::Result<Stop>
where
    C: Console + Keys,
    W: Write,
    V: View,
{
    let (mut cols, mut rows) = size;
    loop {
        view.frame(cols, rows).paint(ui)?;

        let flow = match held.console().key(wait)? {
            Some(Key::Ctrl('c')) => return Ok(Stop::Interrupted),
            Some(Key::Resize(wide, high)) => {
                (cols, rows) = (wide as usize, high as usize);
                Flow::Go
            }
            Some(key) => view.key(key),
            None => view.tick(),
        };

        if flow == Flow::Stop {
            return Ok(Stop::Quit);
        }
    }
}

/// Test Design Specification: the loop a full-screen view runs in.
///
/// Features tested: that the terminal is taken once and given back once on
/// every way out, the failing ways included; that a frame is painted before
/// each wait and only then; that Ctrl-C ends the loop without the view being
/// asked; that a resize changes the size the next frame is built for and is
/// not delivered as a keystroke; that a wait which passes calls `tick` rather
/// than `key`; and that either of them can stop the loop.
///
/// Features NOT tested here: what a frame contains (owned by `frame`), raw
/// mode and the alternate screen themselves (owned by `terminal`, and only
/// exercisable by a process that owns a terminal), and the mapping from a
/// terminal event to a [`Key`] (owned by `terminal`).
///
/// Environmental needs: none. The console is scripted and the frames are
/// painted into a buffer.
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::color::Charset;
    use crate::theme::Theme;
    use crate::writer::buffered;

    use super::*;

    /// A console with its keystrokes written down in advance.
    ///
    /// Running out of them is an error rather than an endless `None`, so a
    /// case whose view forgets to stop fails instead of hanging the suite.
    struct Script {
        keys: std::collections::VecDeque<io::Result<Option<Key>>>,
        did: Rc<RefCell<Vec<&'static str>>>,
        takeable: bool,
    }

    impl Script {
        fn of(keys: Vec<io::Result<Option<Key>>>) -> (Self, Rc<RefCell<Vec<&'static str>>>) {
            let did = Rc::new(RefCell::new(Vec::new()));
            let console = Self {
                keys: keys.into(),
                did: Rc::clone(&did),
                takeable: true,
            };
            (console, did)
        }
    }

    impl Console for Script {
        fn take(&mut self) -> io::Result<()> {
            self.did.borrow_mut().push("take");
            if self.takeable {
                Ok(())
            } else {
                Err(io::Error::other("this terminal cannot be taken"))
            }
        }

        fn restore(&mut self) -> io::Result<()> {
            self.did.borrow_mut().push("restore");
            Ok(())
        }
    }

    impl Keys for Script {
        fn key(&mut self, _: Duration) -> io::Result<Option<Key>> {
            self.keys
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("the script ran out of keystrokes")))
        }
    }

    /// A view that writes down what it was asked, and stops when told.
    #[derive(Default)]
    struct Noted {
        sizes: Vec<(usize, usize)>,
        keys: Vec<Key>,
        ticks: usize,
        /// The key that ends this view, if any.
        quit_on: Option<Key>,
        /// Stop on the nth tick rather than on a key.
        quit_after: Option<usize>,
    }

    impl View for Noted {
        fn frame(&mut self, cols: usize, rows: usize) -> Frame {
            self.sizes.push((cols, rows));
            let mut frame = Frame::new(cols, rows);
            frame.row(format!("frame {}", self.sizes.len()));
            frame
        }

        fn key(&mut self, key: Key) -> Flow {
            self.keys.push(key);
            if self.quit_on == Some(key) {
                Flow::Stop
            } else {
                Flow::Go
            }
        }

        fn tick(&mut self) -> Flow {
            self.ticks += 1;
            if self.quit_after == Some(self.ticks) {
                Flow::Stop
            } else {
                Flow::Go
            }
        }
    }

    fn at(size: (usize, usize)) -> Show {
        Show {
            size,
            // Nothing waits: the script answers immediately either way, and a
            // suite that slept once per keystroke would be paid for on every
            // run of it.
            wait: Duration::ZERO,
        }
    }

    /// Run `view` over `keys`, and report what happened, what the console was
    /// asked to do, and what was painted.
    #[allow(clippy::type_complexity)]
    fn shown(
        keys: Vec<io::Result<Option<Key>>>,
        view: &mut Noted,
        size: (usize, usize),
    ) -> (io::Result<Stop>, Vec<&'static str>, String) {
        let (console, did) = Script::of(keys);
        let mut ui = buffered(Theme::new(false, Charset::Unicode), size.0);
        let stopped = show(console, &mut ui, view, at(size));
        let did = did.borrow().clone();
        (stopped, did, ui.contents())
    }

    /// The number of frames in what was painted. Every frame opens by putting
    /// the cursor home, and nothing else in a frame does.
    fn frames(painted: &str) -> usize {
        painted.matches("\x1b[H").count()
    }

    /// TC-UI-VIEW-1: a view the user quits.
    /// Expected: `Stop::Quit`; the terminal taken once and given back once;
    /// exactly one frame, painted before the keystroke that ended the view.
    /// Painting after the view has stopped would put a frame on a screen that
    /// is about to be left, which is one frame of flicker on the way out.
    #[test]
    fn a_view_that_quits_paints_once_and_gives_the_terminal_back() {
        let mut view = Noted {
            quit_on: Some(Key::Char('q')),
            ..Noted::default()
        };
        let (stopped, did, painted) = shown(vec![Ok(Some(Key::Char('q')))], &mut view, (10, 2));

        assert_eq!(stopped.expect("the loop ends"), Stop::Quit);
        assert_eq!(did, ["take", "restore"]);
        assert_eq!(frames(&painted), 1);
        assert_eq!(view.sizes, [(10, 2)]);
        assert_eq!(view.keys, [Key::Char('q')]);
    }

    /// TC-UI-VIEW-2: Ctrl-C, to a view that never stops on its own.
    /// Expected: `Stop::Interrupted`, the view never asked, and the terminal
    /// back. A view is a program inside a program, and the one thing a person
    /// knows about a terminal is that Ctrl-C gets them out of it - so it
    /// cannot be left to each view to remember.
    #[test]
    fn ctrl_c_ends_the_loop_without_asking_the_view() {
        let mut view = Noted::default();
        let (stopped, did, _) = shown(vec![Ok(Some(Key::Ctrl('c')))], &mut view, (10, 2));

        assert_eq!(stopped.expect("the loop ends"), Stop::Interrupted);
        assert_eq!(did, ["take", "restore"]);
        assert!(view.keys.is_empty(), "{:?}", view.keys);
    }

    /// TC-UI-VIEW-3: the window is resized.
    /// Expected: the next frame is built at the new size, and the view is
    /// never handed the resize as a keystroke. A view that had to unpack the
    /// event itself would be a view that can forget to, and the failure is
    /// silent: it keeps drawing at the old size in a window that is no longer
    /// that size.
    #[test]
    fn a_resize_changes_the_next_frame_and_is_not_a_keystroke() {
        let mut view = Noted {
            quit_on: Some(Key::Char('q')),
            ..Noted::default()
        };
        let (stopped, _, painted) = shown(
            vec![Ok(Some(Key::Resize(20, 4))), Ok(Some(Key::Char('q')))],
            &mut view,
            (10, 2),
        );

        assert_eq!(stopped.expect("the loop ends"), Stop::Quit);
        assert_eq!(view.sizes, [(10, 2), (20, 4)]);
        assert_eq!(view.keys, [Key::Char('q')]);
        assert_eq!(frames(&painted), 2);
        // Two rows in the first frame, four in the second: the row separator
        // appears once fewer than the frame is tall.
        assert_eq!(painted.matches("\r\n").count(), 1 + 3);
    }

    /// TC-UI-VIEW-4: nothing typed for two waits, and the second one ends it.
    /// Expected: `tick` twice, `key` never, three frames - one before each
    /// wait, and the one before the wait that stopped it. This is the path a
    /// view watching a turn arrive spends all of its time on.
    #[test]
    fn a_wait_that_passes_ticks_the_view() {
        let mut view = Noted {
            quit_after: Some(2),
            ..Noted::default()
        };
        let (stopped, did, painted) = shown(vec![Ok(None), Ok(None)], &mut view, (10, 2));

        assert_eq!(stopped.expect("the loop ends"), Stop::Quit);
        assert_eq!(did, ["take", "restore"]);
        assert_eq!(view.ticks, 2);
        assert!(view.keys.is_empty(), "{:?}", view.keys);
        assert_eq!(frames(&painted), 2);
    }

    /// TC-UI-VIEW-5: the loop fails part way through.
    /// Expected: the failure is what the caller gets, and the terminal is
    /// still given back. Reporting the failure on a terminal that is still in
    /// raw mode on the alternate screen means reporting it where nobody will
    /// read it.
    #[test]
    fn a_loop_that_fails_still_gives_the_terminal_back() {
        let mut view = Noted::default();
        let (stopped, did, _) = shown(
            vec![Err(io::Error::other("the terminal went away"))],
            &mut view,
            (10, 2),
        );

        let failed = stopped.expect_err("the loop fails");
        assert_eq!(failed.to_string(), "the terminal went away");
        assert_eq!(did, ["take", "restore"]);
    }

    /// TC-UI-VIEW-6: the terminal cannot be taken.
    /// Expected: the failure is returned and nothing is restored. Restoring
    /// on the off chance would undo a state the process was already in, which
    /// is how a program that failed to start a view leaves the terminal worse
    /// than it found it.
    #[test]
    fn a_terminal_that_cannot_be_taken_is_not_restored() {
        let (mut console, did) = Script::of(Vec::new());
        console.takeable = false;
        let mut ui = buffered(Theme::new(false, Charset::Unicode), 10);
        let mut view = Noted::default();

        let failed = show(console, &mut ui, &mut view, at((10, 2))).expect_err("it fails");

        assert_eq!(failed.to_string(), "this terminal cannot be taken");
        assert_eq!(*did.borrow(), ["take"]);
        assert!(view.sizes.is_empty(), "nothing was painted");
    }
}
