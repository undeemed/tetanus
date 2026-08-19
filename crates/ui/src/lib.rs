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
//! ```
//!
//! Landing in the following slices of this lane: the palette that turns a
//! semantic role into a style, the single writer every line goes through, the
//! help-text surface, and the renderers for a turn's event stream and its
//! progress.
//!
//! # Rationale
//!
//! Policy is its own layer because it fails differently from drawing. It is
//! decided once, from inputs a test can state as plain data, and it never
//! touches a stream. Folding it into a "print helper" is exactly how a CLI
//! ends up with `println!` calls that ignore `NO_COLOR`, and how color becomes
//! a thing you can only test by owning a pty.
//!
//! The crate holds no engine types. It formats what it is given, which is what
//! keeps the presentation lane and the engine lane independently reviewable.

pub mod color;

pub use color::{Charset, ColorChoice, Env};
