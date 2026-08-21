//! One session's events, positioned, and the selections a caller takes of them.

use tetanus_protocol::methods::MAX_PAGE_SIZE;
use tetanus_protocol::types::{KnownEvent, SessionEvent};

use crate::filter::{EventFilter, QueryError, Role};

/// One journal event with everything the fold worked out about where it sits.
///
/// The extra fields are all derivable from the log by a reader willing to make
/// a forward pass; they are here so that reader is written once. Nothing here
/// is stored on disk and nothing here crosses the contract boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    pub event: SessionEvent,
    /// The turn this event happened inside, or `None` for one outside every
    /// turn - the session header, and anything appended between turns.
    pub turn: Option<u64>,
    pub step: Option<u32>,
    pub role: Role,
    /// The tool this event is about: the callee of a `tool/call`, the callee
    /// of the `tool/result` answering one.
    pub tool: Option<String>,
    /// A `tool/result`'s outcome, and `None` on everything else. Not defaulted
    /// to `true`: "did not fail" and "was not a tool result" are different
    /// facts and a count that conflated them would be wrong.
    pub ok: Option<bool>,
    /// The event's own words, for the text clause. `None` where there are none
    /// worth searching: a structural boundary, a stream chunk (whose text
    /// arrives again, whole, on the `assistant/message` that closes it), and
    /// reasoning, which is model-visible but not conversation.
    pub text: Option<String>,
    /// [`Located::text`] folded to lowercase once, so a scan over ten thousand
    /// events does not refold it per comparison.
    search: Option<String>,
}

impl Located {
    pub fn seq(&self) -> u64 {
        self.event.seq
    }

    pub fn ty(&self) -> &str {
        &self.event.ty
    }

    pub fn time(&self) -> u64 {
        self.event.time
    }

    fn matches(&self, filter: &EventFilter) -> bool {
        if let Some(types) = &filter.types {
            if !types.iter().any(|ty| ty == self.ty()) {
                return false;
            }
        }
        if let Some(roles) = &filter.roles {
            if !roles.contains(&self.role) {
                return false;
            }
        }
        if let Some(tools) = &filter.tools {
            match &self.tool {
                Some(tool) => {
                    if !tools.iter().any(|want| want == tool) {
                        return false;
                    }
                }
                // An event with no tool is not one of the tools asked for.
                None => return false,
            }
        }
        if let Some(turns) = &filter.turns {
            match self.turn {
                Some(turn) if turns.contains(turn) => {}
                _ => return false,
            }
        }
        if let Some(steps) = &filter.steps {
            match self.step {
                Some(step) if steps.contains(step) => {}
                _ => return false,
            }
        }
        if let Some(time) = &filter.time {
            if !time.contains(self.time()) {
                return false;
            }
        }
        if let Some(seq) = &filter.seq {
            if !seq.contains(self.seq()) {
                return false;
            }
        }
        if let Some(ok) = filter.ok {
            if self.ok != Some(ok) {
                return false;
            }
        }
        if let Some(text) = &filter.text {
            let needle = text.to_lowercase();
            match &self.search {
                Some(hay) if hay.contains(&needle) => {}
                _ => return false,
            }
        }
        true
    }
}

/// A whole session's events, in seq order, each positioned.
///
/// Built once and read many times: every question this crate answers is a pass
/// over this vector, so a caller that asks five questions pays for one fold.
#[derive(Debug, Clone)]
pub struct Journal {
    session_id: String,
    events: Vec<Located>,
}

impl Journal {
    /// Position a session's events. `events` must be in ascending seq order,
    /// which is the order every source in this workspace produces.
    pub fn new(session_id: impl Into<String>, events: Vec<SessionEvent>) -> Self {
        Self {
            session_id: session_id.into(),
            events: locate(events),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn events(&self) -> &[Located] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The highest turn number the log reached, or `None` on a session that
    /// has not run one.
    pub fn last_turn(&self) -> Option<u64> {
        self.events.iter().filter_map(|event| event.turn).max()
    }

    /// Every event the filter accepts, in seq order.
    ///
    /// The filter is validated first, so an ask that could never match is
    /// refused rather than answered with an empty page a caller would read as
    /// a fact about the session.
    pub fn select(&self, filter: &EventFilter) -> Result<Selection<'_>, QueryError> {
        filter.validate()?;
        Ok(Selection {
            hits: self
                .events
                .iter()
                .filter(|event| event.matches(filter))
                .collect(),
        })
    }
}

/// The answer to one [`Journal::select`].
#[derive(Debug, Clone)]
pub struct Selection<'a> {
    hits: Vec<&'a Located>,
}

impl<'a> Selection<'a> {
    /// How many events matched. The whole count, not the page's - a caller
    /// showing "1-20 of 413" needs the 413 without reading 413 events.
    pub fn count(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a Located> + '_ {
        self.hits.iter().copied()
    }

    pub fn all(&self) -> &[&'a Located] {
        &self.hits
    }

    /// One page, taken by seq rather than by offset.
    ///
    /// By seq for the reason contract section 4.4.5 gives: a page taken by
    /// offset shifts under a log that is still growing, so a pager reading a
    /// live session would skip an event or serve one twice.
    pub fn page(&self, page: Page) -> PageResult<'a> {
        let limit = match page.limit {
            // `0` reads as absent, for the reason section 4.4.5 gives: a page
            // of no events would stall a pager that treats a short page as the
            // end.
            0 => MAX_PAGE_SIZE,
            asked => asked.min(MAX_PAGE_SIZE),
        } as usize;
        let rest: Vec<&'a Located> = self
            .hits
            .iter()
            .copied()
            .filter(|event| event.seq() >= page.from_seq)
            .collect();
        let eof = rest.len() <= limit;
        let events: Vec<&'a Located> = rest.into_iter().take(limit).collect();
        let next_seq = match events.last() {
            Some(last) => last.seq() + 1,
            None => page.from_seq,
        };
        PageResult {
            events,
            next_seq,
            eof,
        }
    }
}

/// Where a page starts and how large it may be. The same two words
/// `session.events` uses, so a caller pages a query the way it pages a log.
/// The default is the whole selection in one page: `from_seq` at the start,
/// and a `limit` of `0`, which asks for the server maximum rather than for no
/// events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    /// First seq to return, inclusive.
    pub from_seq: u64,
    /// Page size, clamped down to [`MAX_PAGE_SIZE`]; `0` asks for that maximum.
    pub limit: u32,
}

impl Page {
    pub fn first(limit: u32) -> Self {
        Self { from_seq: 0, limit }
    }

    pub fn from(from_seq: u64, limit: u32) -> Self {
        Self { from_seq, limit }
    }
}

#[derive(Debug, Clone)]
pub struct PageResult<'a> {
    pub events: Vec<&'a Located>,
    /// `from_seq` for the next page.
    pub next_seq: u64,
    /// True when this page reached the end of the selection.
    pub eof: bool,
}

/// The forward pass: carry the enclosing turn and step onto every event.
///
/// The rule is that an event belongs to the boundary that is open when it is
/// appended. `turn/start` opens a turn and belongs to it; `turn/end` closes one
/// and also belongs to it, which is why the state is cleared *after* the event
/// is placed rather than before. An event that carries its own `turn`/`step` -
/// `assistant/chunk` does - is believed over the running state, because the
/// event is the more specific statement.
fn locate(events: Vec<SessionEvent>) -> Vec<Located> {
    let mut turn: Option<u64> = None;
    let mut step: Option<u32> = None;
    let mut located = Vec::with_capacity(events.len());

    for event in events {
        let parsed = event.parse();
        let mut closes_turn = false;
        let mut closes_step = false;

        match &parsed {
            Some(KnownEvent::TurnStart { turn: n }) => {
                turn = Some(*n);
                step = None;
            }
            Some(KnownEvent::StepStart { turn: t, step: s }) => {
                turn = Some(*t);
                step = Some(*s);
            }
            Some(KnownEvent::StepEnd { turn: t, step: s }) => {
                turn = Some(*t);
                step = Some(*s);
                closes_step = true;
            }
            Some(KnownEvent::TurnEnd { turn: t, .. }) => {
                turn = Some(*t);
                closes_turn = true;
            }
            Some(KnownEvent::AssistantChunk {
                turn: t, step: s, ..
            }) => {
                turn = Some(*t);
                step = Some(*s);
            }
            _ => {}
        }

        let (tool, ok) = tool_of(&parsed);
        let text = text_of(&parsed);
        located.push(Located {
            role: Role::of(&event.ty),
            turn,
            step,
            tool,
            ok,
            search: text.as_deref().map(str::to_lowercase),
            text,
            event,
        });

        if closes_step {
            step = None;
        }
        if closes_turn {
            turn = None;
            step = None;
        }
    }

    located
}

/// The tool an event is about, and its outcome when it has one.
fn tool_of(parsed: &Option<KnownEvent>) -> (Option<String>, Option<bool>) {
    match parsed {
        Some(KnownEvent::ToolCall { name, .. }) => (Some(name.clone()), None),
        Some(KnownEvent::ToolResult { name, ok, .. }) => (Some(name.clone()), Some(*ok)),
        _ => (None, None),
    }
}

/// The words a text clause searches.
///
/// Conversation only. A stream chunk is left out because its text arrives
/// again, whole, on the `assistant/message` that closes the step, and counting
/// it twice would make one sentence look like two hits. Reasoning is left out
/// because it is the model's scratch paper rather than anything it said. A
/// structural boundary has no words at all.
fn text_of(parsed: &Option<KnownEvent>) -> Option<String> {
    match parsed {
        Some(KnownEvent::UserMessage { content }) => Some(content.clone()),
        Some(KnownEvent::AssistantMessage { content, .. }) => Some(content.clone()),
        Some(KnownEvent::ToolResult { content, .. }) => Some(content.clone()),
        Some(KnownEvent::ToolCall {
            name, arguments, ..
        }) => Some(format!("{name} {arguments}")),
        _ => None,
    }
}
