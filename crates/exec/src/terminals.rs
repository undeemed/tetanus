//! The terminals one composition has open, and who may touch them.
//!
//! A terminal session is a live process with a directory, exported variables
//! and a history, so "which session" has to be a question with one answer for
//! the whole life of a turn: a model that opened one two tool calls ago names
//! it by an id this mints and nothing else re-uses.
//!
//! **Sessions have owners, and an owner sees only its own.** Upstream scopes
//! its terminals to the exact `Agent` that opened them, and another agent
//! asking for one is refused. That is not decoration once a harness runs a
//! sub-agent: two agents sharing a registry means one of them typing into a
//! shell the other is halfway through using, and neither can tell. tetanus has
//! no agent identity yet, so an [`Owner`] is an opaque name the composition
//! chooses - one per session for the engine, which is the boundary that exists
//! today - and the scoping is enforced now rather than retrofitted when the
//! identity arrives.
//!
//! **A foreign session is named as foreign, not as missing.** Upstream's
//! `FOREIGN_SESSION` says which of the two happened, and so does this: the
//! boundary here is between parts of one harness, so a caller that reached for
//! somebody else's session has a bug worth reading rather than a secret worth
//! keeping.
//!
//! **Backends are registered by type**, the way upstream registers PTY
//! backends: `bash` today, `pwsh` where a host has one. A request names the
//! type it wants, or takes the first registered.
//!
//! Parity: upstream `packages/terminal/terminal/src/index.ts`
//! (`TerminalSessionService`) and its `tests/service.spec.ts`.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::backend::ShellBackend;
use crate::terminal::{TerminalConfig, TerminalError, TerminalSession};

/// Who a session belongs to.
///
/// Opaque on purpose: the registry compares it and never interprets it, so the
/// day sessions belong to a registered agent this becomes that agent's id
/// without the registry changing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Owner(String);

impl Owner {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn id(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a caller asks for when it opens a terminal.
#[derive(Debug, Clone, Default)]
pub struct OpenRequest {
    /// Which registered backend type. `None` takes the first registered.
    pub kind: Option<String>,
    /// An owner-local display name, so a model can keep two sessions apart by
    /// what they are for rather than by an id it has to remember.
    pub name: Option<String>,
    /// Where the shell starts. Relative paths resolve against the registry's
    /// own working directory, which is the deployment's workspace.
    pub cwd: Option<PathBuf>,
}

/// What closing a session did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    /// This call closed it.
    Now,
    /// Somebody else was already closing it, and this call waited.
    Already,
}

/// One published session and who owns it.
struct Record {
    owner: Owner,
    session: Arc<TerminalSession>,
}

/// The terminals one composition has open.
pub struct Terminals {
    /// Registered backends, in registration order: the first is the default.
    backends: Mutex<Vec<Arc<dyn ShellBackend>>>,
    /// Published sessions, in the order they were opened.
    sessions: Mutex<Vec<Record>>,
    /// Names taken by a session that is still being started, so two opens
    /// racing for one name cannot both win.
    reserved: Mutex<BTreeSet<(Owner, String)>>,
    config: TerminalConfig,
    next: AtomicU64,
}

impl Terminals {
    /// A registry with the defaults every session it opens will start from.
    pub fn new(config: TerminalConfig) -> Self {
        Self {
            backends: Mutex::new(Vec::new()),
            sessions: Mutex::new(Vec::new()),
            reserved: Mutex::new(BTreeSet::new()),
            config,
            next: AtomicU64::new(1),
        }
    }

    /// A registry with one backend on it, which is every deployment that has
    /// not been told about a second.
    pub fn with(
        config: TerminalConfig,
        backend: Arc<dyn ShellBackend>,
    ) -> Result<Self, TerminalError> {
        let terminals = Self::new(config);
        terminals.register_backend(backend)?;
        Ok(terminals)
    }

    /// Offer one backend under its own name as the type callers ask for.
    pub fn register_backend(&self, backend: Arc<dyn ShellBackend>) -> Result<(), TerminalError> {
        let mut backends = self.backends.lock().expect("no panic holds this lock");
        if backends
            .iter()
            .any(|registered| registered.name() == backend.name())
        {
            return Err(TerminalError::DuplicateBackend(backend.name().to_string()));
        }
        backends.push(backend);
        Ok(())
    }

    /// The types a caller may ask for, in registration order.
    pub fn backends(&self) -> Vec<&'static str> {
        self.backends
            .lock()
            .expect("no panic holds this lock")
            .iter()
            .map(|backend| backend.name())
            .collect()
    }

    /// Open one terminal for `owner` and publish it under a fresh id.
    ///
    /// Published only once its shell has reached a prompt, and only if nothing
    /// went wrong: a failed open leaves no id behind, because an id that names
    /// a shell which never started is one every later call fails on.
    pub async fn open(
        &self,
        owner: &Owner,
        request: OpenRequest,
    ) -> Result<Arc<TerminalSession>, TerminalError> {
        let backend = self.backend(request.kind.as_deref())?;
        let kind = backend.name().to_string();
        let _name = self.reserve(owner, request.name.as_deref())?;

        let mut config = self.config.clone();
        if let Some(cwd) = request.cwd {
            config.cwd = if cwd.is_relative() {
                config.cwd.join(cwd)
            } else {
                cwd
            };
        }
        let id = format!("pty-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let session =
            Arc::new(TerminalSession::open(id, request.name.clone(), kind, backend, config).await?);
        self.sessions
            .lock()
            .expect("no panic holds this lock")
            .push(Record {
                owner: owner.clone(),
                session: Arc::clone(&session),
            });
        Ok(session)
    }

    /// The session with this id, if this owner is the one that opened it.
    ///
    /// A session whose shell has died is still answered: the caller is owed
    /// the reason it died, which the session itself carries, rather than "no
    /// such session".
    pub fn get(&self, owner: &Owner, id: &str) -> Result<Arc<TerminalSession>, TerminalError> {
        let sessions = self.sessions.lock().expect("no panic holds this lock");
        let Some(record) = sessions.iter().find(|record| record.session.id() == id) else {
            return Err(TerminalError::NoSession(id.to_string()));
        };
        if &record.owner != owner {
            return Err(TerminalError::Foreign(id.to_string()));
        }
        Ok(Arc::clone(&record.session))
    }

    /// Every session this owner has open, in the order it opened them.
    pub fn list(&self, owner: &Owner) -> Vec<Arc<TerminalSession>> {
        self.sessions
            .lock()
            .expect("no panic holds this lock")
            .iter()
            .filter(|record| &record.owner == owner)
            .map(|record| Arc::clone(&record.session))
            .collect()
    }

    /// Close one session, wait until it and everything on it is gone, and
    /// forget it.
    ///
    /// Answers whether this call was the one that closed it, because "closed"
    /// and "somebody else was already closing it" are different things to tell
    /// a caller and upstream reports the same pair. A session closed and
    /// forgotten answers [`TerminalError::NoSession`] on the call after that,
    /// which is the truth: there is nothing left to close.
    pub async fn kill(&self, owner: &Owner, id: &str) -> Result<Closed, TerminalError> {
        let session = self.get(owner, id)?;
        let already = session.is_closed();
        session.close().await;
        self.sessions
            .lock()
            .expect("no panic holds this lock")
            .retain(|record| record.session.id() != id);
        Ok(if already {
            Closed::Already
        } else {
            Closed::Now
        })
    }

    /// Close everything. What a composition does on the way down, so a run
    /// that ends leaves no terminals behind.
    pub async fn close_all(&self) {
        let all: Vec<Arc<TerminalSession>> = self
            .sessions
            .lock()
            .expect("no panic holds this lock")
            .iter()
            .map(|record| Arc::clone(&record.session))
            .collect();
        for session in all {
            session.close().await;
        }
        self.sessions
            .lock()
            .expect("no panic holds this lock")
            .clear();
    }

    /// The backend a request names, or the first registered.
    fn backend(&self, kind: Option<&str>) -> Result<Arc<dyn ShellBackend>, TerminalError> {
        let backends = self.backends.lock().expect("no panic holds this lock");
        let registered: Vec<String> = backends
            .iter()
            .map(|backend| backend.name().to_string())
            .collect();
        match kind {
            None => backends.first().cloned().ok_or(TerminalError::NoBackend {
                asked: "any".to_string(),
                registered,
            }),
            Some(kind) => backends
                .iter()
                .find(|backend| backend.name() == kind)
                .cloned()
                .ok_or(TerminalError::NoBackend {
                    asked: kind.to_string(),
                    registered,
                }),
        }
    }

    /// Hold a name for the length of an open, and give it back afterwards
    /// however the open went.
    fn reserve(&self, owner: &Owner, name: Option<&str>) -> Result<Reservation<'_>, TerminalError> {
        let Some(name) = name else {
            return Ok(Reservation {
                terminals: self,
                held: None,
            });
        };
        if name.trim().is_empty() {
            return Err(TerminalError::BadName(
                "a terminal session name must not be blank; leave it out instead".into(),
            ));
        }
        let taken = self
            .sessions
            .lock()
            .expect("no panic holds this lock")
            .iter()
            .any(|record| &record.owner == owner && record.session.name() == Some(name));
        let mut reserved = self.reserved.lock().expect("no panic holds this lock");
        if taken || reserved.contains(&(owner.clone(), name.to_string())) {
            return Err(TerminalError::DuplicateName {
                owner: owner.to_string(),
                name: name.to_string(),
            });
        }
        reserved.insert((owner.clone(), name.to_string()));
        Ok(Reservation {
            terminals: self,
            held: Some((owner.clone(), name.to_string())),
        })
    }
}

/// A held name, released when the open that took it finishes either way.
struct Reservation<'a> {
    terminals: &'a Terminals,
    held: Option<(Owner, String)>,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            self.terminals
                .reserved
                .lock()
                .expect("no panic holds this lock")
                .remove(&held);
        }
    }
}
