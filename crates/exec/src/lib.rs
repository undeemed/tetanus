//! Process execution: the seam everything that has to leave the harness runs
//! through.
//!
//! - [`proc`] runs one external command - argv without a shell re-split, an
//!   environment the caller listed, a working directory, captured stdio, an
//!   exit status or a signal, incremental output, and a termination that
//!   reaches the whole process group rather than one child.
//!
//! Parity: upstream `packages/subprocess`, `packages/shell` and
//! `packages/terminal`, restated against this seam. `docs/parity.md` records
//! what is served and what is not.

pub mod proc;

pub use proc::{
    Captured, Chunk, Collected, Command, Ending, Limits, Output, OutputSink, ProcessError, Stream,
};
