//! The session store: journals on disk, live sessions in memory, and the
//! `session.*` reads the contract defines over both.
//!
//! A journal is self-describing. Its first line is a `session/start` event
//! carrying the header, so listing cold sessions reads the log and never a
//! sidecar file. Everything a surface sees about a session is therefore one
//! append-only artifact, which is the same rule the turn flow follows.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tetanus_core::EventBus;
use tetanus_protocol::methods::{SessionCreateParams, SessionEventsResult, SessionForkParams};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::{AgentState, SessionInfo};
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};

use crate::convert::{session_error, session_event, session_not_found};

/// The durable header event every journal opens with.
pub const SESSION_START: &str = "session/start";

/// Longest `SessionInfo.title` the engine reports. A picker gets a line, not
/// a paragraph; the whole message is one `session.events` call away.
pub const MAX_TITLE: usize = 80;

/// Largest page `session.events` returns, and the page size when a caller
/// names none. A caller that wants the whole journal pages to `eof`.
///
/// The value is the contract's, not this module's: `session.events` clamps to
/// what a surface can read, so a build that changed one and not the other
/// would answer pages a caller was told it would not get.
pub const MAX_PAGE: u32 = tetanus_protocol::methods::MAX_PAGE_SIZE;

/// The `session/start` payload: what a surface needs to list a cold session.
///
/// Lineage is part of it, so a forked journal says where it came from without
/// a sidecar. Both fields are absent on a session that was opened rather than
/// forked, which is what keeps every journal written before forking existed
/// readable by this reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub max_steps: u32,
    /// The session this journal was forked from (contract section 4.4.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    /// Last parent seq this journal inherited, inclusive. Present exactly when
    /// `parent_session` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_seq: Option<u64>,
}

/// One session held open in memory. Slice-scoped: the turn runtime attaches
/// its own state to this handle.
pub struct LiveSession {
    pub header: SessionHeader,
    pub path: PathBuf,
    pub log: Arc<JsonlSessionLog>,
    /// One bus per session, so a push carries the session it belongs to
    /// without the hub having to demultiplex a shared stream.
    pub bus: EventBus,
}

/// Defaults a `session.create` with no overrides resolves against.
#[derive(Debug, Clone)]
pub struct SessionDefaults {
    pub provider: String,
    pub model: String,
    pub max_steps: u32,
}

pub struct SessionStore {
    root: PathBuf,
    defaults: SessionDefaults,
    live: Mutex<BTreeMap<String, Arc<LiveSession>>>,
    counter: AtomicU64,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>, defaults: SessionDefaults) -> Self {
        Self {
            root: root.into(),
            defaults,
            live: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    /// Open a session, creating its journal when the id is new. An id that
    /// already has a journal is reopened, so a surface can resume.
    pub fn create(&self, params: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        // A named path is opened where it is. Its id then comes from its own
        // `session/start` line, which is how a path becomes an id for every
        // other call.
        let named = params.path.as_deref().map(PathBuf::from);
        let carried = match &named {
            Some(path) => header_of(&tetanus_session::replay(path).map_err(|e| {
                session_error(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""), e)
            })?)
            .map(|header| header.session_id),
            None => None,
        };

        let id = match carried.or(params.session_id) {
            Some(id) => {
                validate_id(&id)?;
                id
            }
            None => self.fresh_id(),
        };
        if let Some(live) = self.live.lock().expect("live").get(&id) {
            return Ok(self.info(live));
        }

        let path = named
            .or_else(|| self.resolve(&id))
            .unwrap_or_else(|| self.path_of(&id));
        let existing = tetanus_session::replay(&path).map_err(|e| session_error(&id, e))?;

        let bus = EventBus::new();
        let log =
            JsonlSessionLog::create(&id, &path, bus.clone()).map_err(|e| session_error(&id, e))?;

        // A turn a crash left open is closed before anything derives history
        // from this journal: a dangling tool call is not a history a provider
        // accepts. A balanced journal is untouched.
        tetanus_turn::repair::repair(log.as_ref()).map_err(|e| session_error(&id, e))?;

        // A reopened journal keeps the header it was created with: the model a
        // turn already ran under is a fact of the log, not of this call.
        let header = match header_of(&existing) {
            Some(header) => header,
            None => {
                let header = SessionHeader {
                    session_id: id.clone(),
                    provider: params
                        .provider
                        .unwrap_or_else(|| self.defaults.provider.clone()),
                    model: params.model.unwrap_or_else(|| self.defaults.model.clone()),
                    max_steps: params.max_steps.unwrap_or(self.defaults.max_steps),
                    parent_session: None,
                    fork_seq: None,
                };
                let value = serde_json::to_value(&header).map_err(crate::convert::internal)?;
                log.append(SESSION_START, value)
                    .map_err(|e| session_error(&id, e))?;
                header
            }
        };

        let live = Arc::new(LiveSession {
            header,
            path,
            log,
            bus,
        });
        let info = self.info(&live);
        self.live.lock().expect("live").insert(id, live);
        Ok(info)
    }

    /// Open a child session seeded with a prefix of another one's journal
    /// (contract section 4.4.6).
    ///
    /// The child is a copy: the parent is read and never written. Its journal
    /// is the inherited prefix with the parent's `session/start` replaced, one
    /// line for one line, by the child's own - which is why the copied events
    /// keep their seqs and their `sourceEventSeqs` need no rewriting. Nothing
    /// ever cites seq 0.
    pub fn fork(&self, params: SessionForkParams) -> Result<SessionInfo, RpcError> {
        // The child id is settled first, as upstream settles it: a caller that
        // named an id it already holds is told that, whatever else is also
        // wrong with the request. Refused rather than reopened, because a seed
        // written onto a journal that already holds a history would splice the
        // two together.
        let child_id = match params.child_session_id {
            Some(id) => {
                validate_id_named(&id, "child_session_id")?;
                if self.resolve(&id).is_some() {
                    return Err(RpcError::new(
                        ErrorCode::InvalidParams,
                        format!("session `{id}` already exists"),
                    )
                    .with_data(serde_json::json!({ "field": "child_session_id" })));
                }
                id
            }
            None => self.fresh_id(),
        };

        // A journal with no header is not a session this server can open: it
        // is the one `session.list` already declines to report, and there is
        // no route for the child to inherit.
        let events = self.read_all(&params.session_id)?;
        let Some(parent) = header_of(&events) else {
            return Err(session_not_found(&params.session_id));
        };
        let last = events.len() as u64 - 1;
        let boundary = params.through_seq.unwrap_or(last);
        if boundary > last {
            return Err(bad_boundary(format!(
                "fork boundary {boundary} does not exist in session `{}` (last seq: {last})",
                params.session_id
            )));
        }
        if let Some(turn) = open_turn_at(&events, boundary) {
            return Err(bad_boundary(format!(
                "fork boundary {boundary} in session `{}` ends inside open turn {turn}",
                params.session_id
            )));
        }

        let header = SessionHeader {
            session_id: child_id.clone(),
            // The route comes with the history: what the child inherits was
            // produced under the parent's provider and model.
            provider: parent.provider,
            model: parent.model,
            max_steps: parent.max_steps,
            parent_session: Some(parent.session_id),
            fork_seq: Some(boundary),
        };
        let mut seed = Vec::with_capacity(boundary as usize + 1);
        seed.push(SessionEvent {
            ty: SESSION_START.to_string(),
            seq: 0,
            time: now_ms(),
            data: serde_json::to_value(&header).map_err(crate::convert::internal)?,
            source_event_seqs: None,
        });
        seed.extend(events[1..=boundary as usize].iter().cloned());

        let path = self.path_of(&child_id);
        tetanus_session::seed(&path, &seed).map_err(|e| session_error(&child_id, e))?;

        // Opened through the ordinary path, so a forked session is live and
        // listed on exactly the terms every other session is.
        self.create(SessionCreateParams {
            session_id: Some(child_id),
            ..SessionCreateParams::default()
        })
    }

    /// Every session this store knows: the live ones, plus every journal on
    /// disk that a restart left behind.
    pub fn list(&self) -> Result<Vec<SessionInfo>, RpcError> {
        let live = self.live.lock().expect("live");
        let mut out: BTreeMap<String, SessionInfo> = live
            .values()
            .map(|s| (s.header.session_id.clone(), self.info(s)))
            .collect();

        for path in self.journals()? {
            // A journal that cannot be read is skipped rather than failing
            // the whole listing: one damaged session must not hide the
            // others.
            let events = tetanus_session::replay(&path).unwrap_or_default();
            let Some(header) = header_of(&events) else {
                continue;
            };
            // Keyed by the id it reports, and not by the file name, because
            // the two may differ and only the reported one is an id the rest
            // of `session.*` resolves.
            if out.contains_key(&header.session_id) {
                continue;
            }
            out.insert(
                header.session_id.clone(),
                SessionInfo {
                    session_id: header.session_id,
                    path: path.display().to_string(),
                    provider: header.provider,
                    model: header.model,
                    created_time: events.first().map_or(0, |e| e.time),
                    last_seq: events.len() as i64 - 1,
                    title: title_of(&events),
                    state: AgentState::Idle,
                },
            );
        }
        Ok(out.into_values().collect())
    }

    /// One page of a journal, by seq. Paging by seq and not by offset is what
    /// makes a page stable while the log grows underneath it.
    pub fn events(
        &self,
        session_id: &str,
        from_seq: u64,
        limit: Option<u32>,
    ) -> Result<SessionEventsResult, RpcError> {
        let events = self.read_all(session_id)?;
        // Contract section 4.4.5: zero reads as absent. A page of no events
        // stalls a pager - `next_seq` would not advance and `eof` would stay
        // false - so the one page size a caller cannot mean is not served.
        let limit = limit.filter(|n| *n > 0).unwrap_or(MAX_PAGE).min(MAX_PAGE) as usize;
        let start = (from_seq as usize).min(events.len());
        let end = start.saturating_add(limit).min(events.len());
        Ok(SessionEventsResult {
            events: events[start..end]
                .iter()
                .cloned()
                .map(session_event)
                .collect(),
            next_seq: end as u64,
            eof: end == events.len(),
        })
    }

    /// The live handle for a session, opening its journal if this process has
    /// not already. A subscriber names a session it did not create, and a cold
    /// journal has a bus to attach to as soon as it is open.
    pub fn open(&self, session_id: &str) -> Result<Arc<LiveSession>, RpcError> {
        if let Some(live) = self.live(session_id) {
            return Ok(live);
        }
        validate_id(session_id)?;
        if self.resolve(session_id).is_none() {
            return Err(session_not_found(session_id));
        }
        self.create(SessionCreateParams {
            session_id: Some(session_id.to_string()),
            ..SessionCreateParams::default()
        })?;
        self.live(session_id).ok_or_else(|| {
            crate::convert::internal(format!("session `{session_id}` vanished while opening"))
        })
    }

    /// The live handle for a session, for callers that run turns on it.
    pub fn live(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        self.live.lock().expect("live").get(session_id).cloned()
    }

    /// A live session reads from memory; a cold one is replayed from disk, so
    /// a surface can page a journal this process never opened.
    fn read_all(&self, session_id: &str) -> Result<Vec<SessionEvent>, RpcError> {
        if let Some(live) = self.live(session_id) {
            return Ok(live.log.events());
        }
        validate_id(session_id)?;
        let Some(path) = self.resolve(session_id) else {
            return Err(session_not_found(session_id));
        };
        tetanus_session::replay(&path).map_err(|e| session_error(session_id, e))
    }

    fn info(&self, live: &LiveSession) -> SessionInfo {
        let events = live.log.events();
        SessionInfo {
            session_id: live.header.session_id.clone(),
            path: live.path.display().to_string(),
            provider: live.header.provider.clone(),
            model: live.header.model.clone(),
            created_time: events.first().map_or(0, |e| e.time),
            last_seq: events.len() as i64 - 1,
            title: title_of(&events),
            state: AgentState::Idle,
        }
    }

    /// Whether this store is rooted where a caller that named no directory
    /// would have written. `./sessions` is the same place as `sessions`, so
    /// the comparison drops the components that mean nothing rather than
    /// demanding one spelling.
    fn root_is_default(&self) -> bool {
        fn meaningful(path: &Path) -> impl Iterator<Item = Component<'_>> {
            path.components()
                .filter(|c| !matches!(c, Component::CurDir))
        }
        meaningful(&self.root).eq(meaningful(Path::new(crate::DEFAULT_SESSIONS_ROOT)))
    }

    fn journals(&self) -> Result<Vec<PathBuf>, RpcError> {
        let dir = match std::fs::read_dir(&self.root) {
            Ok(dir) => dir,
            // The default root is allowed not to exist: a build that has never
            // run a turn has not created it, and "no sessions yet" is the true
            // answer. A root the caller named is not allowed to be missing.
            // Reading a typo there as an empty history tells the reader to run
            // a turn when what they have to do is fix the path, and no surface
            // can tell the two apart from an empty list.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.root_is_default() => {
                return Ok(Vec::new())
            }
            Err(e) => return Err(crate::convert::io_error(&self.root, &e)),
        };
        let mut out: Vec<PathBuf> = dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        out.sort();
        Ok(out)
    }

    fn path_of(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.jsonl"))
    }

    /// The journal an id names, or `None` if this store holds none.
    ///
    /// Contract section 4.7: an id is a fact of the journal's `session/start`
    /// line, not of its file name, so `<root>/<id>.jsonl` is a fast path and
    /// not the definition. An id the store minted for a journal that is called
    /// something else still resolves, which is what makes every id
    /// `session.list` reports an id `session.events` can open.
    fn resolve(&self, id: &str) -> Option<PathBuf> {
        if let Some(live) = self.live(id) {
            return Some(live.path.clone());
        }
        let direct = self.path_of(id);
        if direct.exists() {
            match header_at(&direct) {
                // The ordinary case, and a journal whose header is not written
                // yet: the file name is all there is to go on.
                None => return Some(direct),
                Some(header) if header.session_id == id => return Some(direct),
                // Named `<id>.jsonl` but belonging to another session. The id
                // wins, so the search goes on.
                Some(_) => {}
            }
        }
        self.journals()
            .ok()?
            .into_iter()
            .find(|path| header_at(path).is_some_and(|header| header.session_id == id))
    }

    fn fresh_id(&self) -> String {
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        loop {
            let n = self.counter.fetch_add(1, Ordering::Relaxed);
            let id = if n == 0 {
                format!("s{base}")
            } else {
                format!("s{base}-{n}")
            };
            if self.resolve(&id).is_none() {
                return id;
            }
        }
    }
}

fn header_of(events: &[SessionEvent]) -> Option<SessionHeader> {
    let first = events.first()?;
    if first.ty != SESSION_START {
        return None;
    }
    serde_json::from_value(first.data.clone()).ok()
}

/// The session's first user message, cut to one line. A picker that had to
/// page every journal for this would be reading the engine's side of the
/// boundary, which is why the engine reports it.
fn title_of(events: &[SessionEvent]) -> Option<String> {
    let first = events
        .iter()
        .find(|e| e.ty == "user/message")?
        .data
        .get("content")?
        .as_str()?
        .trim();
    if first.is_empty() {
        return None;
    }
    let line = first.lines().next().unwrap_or(first);
    match line.char_indices().nth(MAX_TITLE) {
        Some((cut, _)) => Some(format!("{}...", &line[..cut])),
        None => Some(line.to_string()),
    }
}

/// The header of a cold journal, read from its first line alone. Resolving one
/// id must not cost a full replay of every journal in the directory.
fn header_at(path: &Path) -> Option<SessionHeader> {
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let event: SessionEvent = serde_json::from_str(first.trim_end()).ok()?;
    header_of(&[event])
}

/// The turn a boundary falls inside, or `None` when the prefix ending there is
/// closed.
///
/// Stated over the log and not over live state: the last `turn/start` or
/// `turn/end` at or before the boundary decides. A `turn/start` means the turn
/// it opened is still open there, so a child seeded with that prefix would
/// begin owing a result to a turn that never ran on it.
fn open_turn_at(events: &[SessionEvent], boundary: u64) -> Option<u64> {
    let last = events[..=boundary as usize]
        .iter()
        .rev()
        .find(|e| e.ty == "turn/start" || e.ty == "turn/end")?;
    if last.ty != "turn/start" {
        return None;
    }
    Some(last.data.get("turn").and_then(|t| t.as_u64()).unwrap_or(0))
}

fn bad_boundary(message: String) -> RpcError {
    RpcError::new(ErrorCode::InvalidParams, message)
        .with_data(serde_json::json!({ "field": "through_seq" }))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A session id names a file, so it may not reach outside the store's root.
fn validate_id(id: &str) -> Result<(), RpcError> {
    validate_id_named(id, "session_id")
}

/// The same rule, reported against the parameter that carried the id, so a
/// caller that named a child id is told which of two ids was refused.
fn validate_id_named(id: &str, field: &str) -> Result<(), RpcError> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && id != "."
        && id != "..";
    if ok {
        Ok(())
    } else {
        Err(RpcError::new(
            ErrorCode::InvalidParams,
            format!("{field} must be 1 to 128 characters of [A-Za-z0-9._-]"),
        )
        .with_data(serde_json::json!({ "field": field })))
    }
}
