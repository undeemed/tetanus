//! Reading a picture out of the workspace without carrying it into the turn.
//!
//! **The bytes never reach the model.** A picture is admitted to a store and
//! what comes back is a name, a size, a media type and an id - which is
//! `docs/interface-contract.md` §5.1's rule for a view, applied one layer
//! down: a base64 image in a tool result is a `tool/call` nobody can grep, a
//! journal line nobody can read, and a context window spent on something the
//! model asked to *look at* rather than to *quote*.
//!
//! **The store is a seam, not a dependency.** This crate does not know what an
//! attachment store is; [`ImageSink`] is one method, and the composition
//! supplies the implementation. That is deliberate rather than tidy:
//! `ARCHITECTURE.md` §4.2 says nothing depends on `tetanus-fs` because it is a
//! consumer of the tool seam and not a layer under it, and a `crates/fs` that
//! reached into the crate holding the store would make the file tools
//! unavailable to any composition that did not also want the feature tools.
//!
//! **The fence judges an image read exactly as it judges any other.** The
//! bytes come through [`crate::service::FileSystem::read_bytes`], so a picture
//! outside the workspace is refused by the same rule that refuses a source
//! file outside it, and the Landlock worker sees the read the same way.

use std::sync::Arc;

/// What a store answers when it has taken a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The content address the store keeps it under. A surface fetches the
    /// bytes by this; equal bytes give an equal id, so a second read of the
    /// same picture costs nothing.
    pub id: String,
    /// What the store decided this is - `image/png` and the rest - rather than
    /// what the file extension claimed.
    pub media_type: String,
    pub bytes: usize,
    /// Present when the store could measure the picture from its header.
    pub dimensions: Option<(u32, u32)>,
}

/// Where a picture goes when a tool reads one.
///
/// One method, because one method is the whole contract: hand over bytes and a
/// name, receive what the store made of them. Everything a store does that is
/// interesting - content addressing, deduplication, header measurement, its
/// own limits - happens on the other side of this and is none of this crate's
/// business.
pub trait ImageSink: Send + Sync {
    /// Take these bytes, or say why they are not admissible.
    ///
    /// The error is a sentence for the model rather than a class, because
    /// every way this fails is something the model can act on directly: the
    /// picture is too large, the format is not one this build reads, the store
    /// could not write. A code would be a second vocabulary for a decision the
    /// tool layer already renders in words.
    fn admit(&self, name: &str, bytes: Vec<u8>) -> Result<Stored, String>;
}

/// A sink that refuses everything, naming what a composition did not supply.
///
/// Composed rather than absent, so `read_image` is a tool that explains itself
/// instead of a tool that vanished: a model offered a picture-reading tool
/// that is not there learns nothing, and a build that quietly dropped it looks
/// to its author exactly like one that never had it.
pub struct NoSink;

impl ImageSink for NoSink {
    fn admit(&self, _name: &str, _bytes: Vec<u8>) -> Result<Stored, String> {
        Err(
            "this harness has no attachment store, so a picture cannot be kept; \
             read the file with `read` if it is text"
                .into(),
        )
    }
}

/// The sink the file tools were composed with.
pub type SharedSink = Arc<dyn ImageSink>;
