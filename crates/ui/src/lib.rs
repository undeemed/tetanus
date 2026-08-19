//! `tetanus-ui` - the presentation layer of the `tetanus` binary.
//!
//! # Identification
//!
//! Component: presentation layer, tetanus 0.1.0, Phase ① skeleton, in
//! progress. Authoritative copy: `crates/ui` in the tetanus repository.
//!
//! # Concerns this crate answers
//!
//! - *A user at a terminal*: is the output readable, aligned, and honest about
//!   what happened?
//! - *A user piping into a file or into CI*: is the output plain, stable, and
//!   free of escape codes?
//! - *A renderer author*: where do I write, and how do I name a color I am not
//!   allowed to choose?
//!
//! # Composition
//!
//! ```text
//! color   ── policy: --color + environment + is-terminal → color on/off, charset, width
//! theme   ── palette: Role → anstyle::Style, gated by that policy
//! writer  ── Ui<W>: the only place a line is written; owns stream + theme + width
//! frame   ── Frame: one whole screen, composed and painted in a single pass
//! page    ── Page: a Frame with a kept transcript and a window back through it
//! progress ─ Progress<W>: the one status line, animated only at a terminal
//! screen  ── Screen<W>: a block of lines redrawn in place, at a terminal only
//! terminal ─ Held<C>: raw mode and the alternate screen, taken and given back
//! view    ── show(): the loop a full-screen view runs in, terminal and all
//! ```
//!
//! Landing in the following slices of this lane: the help-text surface, and
//! the renderers for a turn's event stream and its progress.
//!
//! # Rationale
//!
//! The three layers are separate because they fail differently. Policy is
//! decided once, from inputs a test can state as plain data, and never touches
//! a stream. The palette is a pure mapping with no I/O. The writer is the
//! single I/O choke point, which is what stops a renderer from reaching
//! `println!` and escaping the policy. Folding them into one "print helper" is
//! exactly how a CLI ends up ignoring `NO_COLOR`, and how color becomes a
//! thing you can only test by owning a pty.
//!
//! The crate holds no engine types. It formats what it is given, which is what
//! keeps the presentation lane and the engine lane independently reviewable.

pub mod color;
pub mod frame;
pub mod page;
pub mod progress;
pub mod screen;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod view;
pub mod writer;

pub use color::{Charset, ColorChoice, Env};
pub use frame::Frame;
pub use page::{bar, Page};
pub use progress::Progress;
pub use screen::Screen;
pub use terminal::{when_killed, Console, Held, Key, Keys, Killed, Tty};
pub use text::{fit, light, or_empty, plain, tame, tame_line, truncate, visible_width, wrap};
pub use theme::{Painted, Role, Theme};
pub use view::{show, size, Flow, Show, Stop, View};
pub use writer::{buffered, measure, Policy, Ui};
