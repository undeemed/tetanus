//! Where a journal is read from.
//!
//! One trait with one call, because that is all reading a journal needs: hand
//! me a page from this seq. The engine already serves exactly that as
//! `session.events`, so the in-process reader and a reader on the far end of a
//! carrier load the same way and this crate never learns which it is.

use std::sync::Arc;

use tetanus_protocol::methods::{Engine, SessionEventsParams, SessionEventsResult};
use tetanus_protocol::rpc::RpcError;

use crate::filter::QueryError;
use crate::journal::Journal;

/// A paged reader over one session's journal.
#[async_trait::async_trait]
pub trait EventSource: Send + Sync {
    async fn page(
        &self,
        session_id: &str,
        from_seq: u64,
    ) -> Result<SessionEventsResult, QueryError>;
}

#[async_trait::async_trait]
impl EventSource for Arc<dyn Engine> {
    async fn page(
        &self,
        session_id: &str,
        from_seq: u64,
    ) -> Result<SessionEventsResult, QueryError> {
        Engine::session_events(
            self.as_ref(),
            SessionEventsParams {
                session_id: session_id.to_string(),
                from_seq,
                // Absent, so the server's own maximum applies. Naming a number
                // here would be this crate asserting a page size for a server
                // it may not be talking to.
                limit: None,
            },
        )
        .await
        .map_err(QueryError::Source)
    }
}

impl Journal {
    /// Read a whole session through a source and position it.
    ///
    /// Pages until the source says `eof`, which is the only termination
    /// condition that is correct for both a fixed log and one still being
    /// written: a short page is not the end, and the server says so.
    ///
    /// A source that answers `eof: false` while returning no events would spin,
    /// so that is treated as the end too. It is a source defect either way, and
    /// a hang is the one failure a caller cannot diagnose.
    pub async fn load(source: &dyn EventSource, session_id: &str) -> Result<Journal, QueryError> {
        let mut events = Vec::new();
        let mut from_seq = 0;
        loop {
            let page = source.page(session_id, from_seq).await?;
            let empty = page.events.is_empty();
            events.extend(page.events);
            if page.eof || empty {
                break;
            }
            from_seq = page.next_seq;
        }
        Ok(Journal::new(session_id, events))
    }
}

/// Read a session through the engine facade. The common case, spelled once so
/// a caller does not have to name the trait object.
pub async fn load(engine: &Arc<dyn Engine>, session_id: &str) -> Result<Journal, RpcError> {
    Journal::load(engine, session_id)
        .await
        .map_err(RpcError::from)
}
