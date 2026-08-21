//! The seam between the protocol and whatever carries it.
//!
//! A link is two halves rather than one object, because the driver in
//! [`crate::connection`] reads and writes at the same time: one task waits on
//! the peer's output while the task holding the commands writes to its input.
//! One object with both methods would have to be locked around every read,
//! and a read waits for as long as the peer is quiet.
//!
//! The writing half also owns *stopping* the peer, because on stdio those are
//! the same resource: closing the child's input is the polite half of the
//! ladder that ends in a kill.

use std::io;

/// How a peer went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// A pipe with no process behind it, closed.
    Closed,
    /// The child exited on its own, with this status.
    Code(i32),
    /// The child was still running when its grace period ran out, and was
    /// killed.
    Killed,
    /// The child ended, and the platform gave no code - a signal, usually.
    Unknown,
}

/// What closing a link had to do, reported rather than assumed.
///
/// A test asserts on this: "no orphan process is left behind" is a claim, and
/// this is the evidence. [`Departure::pid`] is the process that is gone, so a
/// caller can check the system's view too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Departure {
    pub exit: Exit,
    pub pid: Option<u32>,
}

impl Departure {
    pub fn closed() -> Self {
        Self {
            exit: Exit::Closed,
            pid: None,
        }
    }
}

/// The half a client writes to, and stops.
#[async_trait::async_trait]
pub trait LinkWriter: Send {
    /// Send one framed message. The newline is this method's to add, so no
    /// caller can forget it.
    async fn send(&mut self, line: &str) -> io::Result<()>;

    /// Stop the peer, and do not return until it is gone.
    ///
    /// Bounded by whatever grace the implementation was built with: a peer
    /// that will not leave is killed, because a client that waits for ever on
    /// a shutdown is how orphans happen.
    async fn stop(&mut self) -> Departure;
}

/// The half a client reads from.
#[async_trait::async_trait]
pub trait LinkReader: Send {
    /// The next line the peer sent, without its newline. `None` is end of
    /// stream: the peer will send nothing more.
    async fn recv(&mut self) -> Option<io::Result<String>>;
}

/// One live server, as the connection driver takes it.
pub struct Link {
    pub writer: Box<dyn LinkWriter>,
    pub reader: Box<dyn LinkReader>,
    /// The process behind it, when there is one. Stated at the point the link
    /// is made, because a caller that has to prove nothing was left behind
    /// needs the pid whether or not the conversation ever got going.
    pub pid: Option<u32>,
}

impl Link {
    pub fn new(writer: Box<dyn LinkWriter>, reader: Box<dyn LinkReader>) -> Self {
        Self {
            writer,
            reader,
            pid: None,
        }
    }

    pub fn of_process(
        writer: Box<dyn LinkWriter>,
        reader: Box<dyn LinkReader>,
        pid: Option<u32>,
    ) -> Self {
        Self {
            writer,
            reader,
            pid,
        }
    }
}
