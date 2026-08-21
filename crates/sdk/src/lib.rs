//! Driving tetanus from Rust, without a process boundary in the way.
//!
//! A caller that wants a turn has three ways in today: spawn the binary and
//! parse it, open a carrier and speak JSON-RPC to it, or import
//! `tetanus_protocol::methods::Engine` and hand-roll the handshake, the
//! subscription and the event collection that a turn needs. The third is what
//! every in-repository caller actually does, and each does it slightly
//! differently. This crate is that work, done once and typed.
//!
//! Two layers, for the reason upstream's SDK has two.
//!
//! [`Client`] is the protocol client: one method per contract call, the
//! handshake enforced exactly as a carrier enforces it, subscriptions that the
//! client closes on the way out. It waits for nothing it was not asked to wait
//! for.
//!
//! [`Harness`] is the owned-run API over it: [`Session::run`] subscribes,
//! prompts, and hands back the turn's summary together with every event the
//! turn produced. That collection is the part worth owning centrally - the
//! subscription has to be open *before* the prompt or the first events are
//! gone, and that is an ordering bug a caller writes once and debugs for a
//! week.
//!
//! [`gateway`] is the other side of the same contract: the request surface
//! restated as data. The codec's `match` over method names cannot be
//! enumerated, so nothing today can answer "what calls are there and what
//! arguments do they take". The gateway can, and validates a call's named
//! arguments against that answer before dispatching it.
//!
//! No CLI, no subprocess, no wire. The client holds an `Arc<dyn Engine>` and
//! calls it, so a test drives the same code path a carrier drives, and the two
//! cannot serve different contracts.
//!
//! ```no_run
//! # async fn example(engine: std::sync::Arc<dyn tetanus_protocol::methods::Engine>)
//! # -> Result<(), tetanus_sdk::SdkError> {
//! use tetanus_sdk::Harness;
//!
//! let harness = Harness::new(engine);
//! let session = harness.session().await?;
//! let run = session.run("say hi").await?;
//! println!("{}", run.final_response());
//! # Ok(()) }
//! ```

pub mod client;
pub mod events;
pub mod gateway;
pub mod harness;

/// Reading a session as data. Re-exported rather than left to the caller so an
/// SDK consumer needs one dependency, and so the filter types a `Journal`
/// answers questions about cannot come from a different version of the crate
/// that built it.
pub use tetanus_query as query;

pub use client::{Client, SdkError};
pub use events::{Subscription, Update};
pub use gateway::{Gateway, InvocationDescriptor, ParamSpec};
pub use harness::{Harness, RunResult, Session};

/// The name this SDK introduces itself with in the handshake.
///
/// Fixed here rather than at each call site because a server that logs its
/// peers should see one name for every caller of this crate, not one per
/// caller of this crate's callers.
pub const CLIENT_NAME: &str = "tetanus-sdk";
