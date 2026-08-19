//! The presentation half of the binary: a value in, terminal output out.
//!
//! Nothing here calls the engine, and nothing here decides a colour on its
//! own - the palette and the writer both come from `tetanus-ui`. Keeping the
//! two halves in separate directories is what lets the engine lane and the
//! presentation lane change the binary without reviewing each other's work.

pub mod help;
pub mod timeline;
