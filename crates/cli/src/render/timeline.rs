//! A turn's events as something a person reads.
//!
//! The input is a stream of `tetanus-protocol` events; the output is lines.
//! Nothing here knows where the events came from, so the same renderer serves
//! a journal on disk, an in-process turn, and a WebSocket subscription.
//!
//! Composing a line and writing it are separate on purpose. [`Reader`] turns
//! one event into the lines it produces and writes nothing; [`render`] is the
//! reader of a finished turn, and hands those lines to a `Ui`. A live view
//! keeps some of them on screen and rewrites them as the turn goes on, and it
//! has to reach the same words this reader would - two composers would drift
//! within a slice or two.
//!
//! Two rendering decisions worth naming.
//!
//! `assistant/chunk` is not drawn. The chunks are the streaming surface, and
//! the `assistant/message` that follows carries the same text assembled; a
//! reader of a finished turn wants the sentence, not the sixty pieces it
//! arrived in. A live surface renders the chunks instead and skips the
//! assembled message - same events, different reader.
//!
//! A `tool/result` is paired to its `tool/call` by `call_id` and never by
//! arrival order, per contract §4.3.1. When the pairing is not the obvious one
//! the line says which call it answers, so two tool calls in flight stay
//! readable.
//!
//! Thinking is folded to one line. A reasoning model spends hundreds of lines
//! deciding, and printing all of them buries the answer it decided on -
//! upstream's conversation view collapses the same text behind a disclosure
//! row, and shows the first line of it. `--think` is the CLI's way of opening
//! that row.
//!
//! What a call was given and what it produced are laid out differently. The
//! arguments are JSON a reader recognises rather than reads, so they are cut
//! to the line like any other value. The result is the work of the turn - a
//! file, a command's output, a search - so it is folded to the width and read
//! like prose, capped in height the way upstream caps a terminal card.
//!
//! Every value in an event came off the wire, the short ones included. A
//! message and a result are tamed by the width rules that size them
//! ([`wrap`], [`truncate`]), but a tool's name, an event's type, a model, a
//! `call_id` and a stop reason are drawn as they arrived, so this module tames
//! them itself. A journal is a file, and a file is something a reader can be
//! sent.

use std::io::{self, Write};
use std::time::Duration;

use tetanus_protocol::types::{KnownEvent, SessionEvent, StopReason, Usage};
use tetanus_ui::{tame, truncate, visible_width, wrap, Role, Theme, Ui};

/// Where a content line starts: two of indent, a five-column label, two more.
pub(super) const LABEL: usize = 5;
pub(super) const INDENT: &str = "  ";

/// Lines of a tool's result drawn before the middle of it is folded away.
///
/// Sixteen, and sliced head and tail, because that is what upstream's terminal
/// card does (`headTailCap` in `packages/client/ui-primitives`): the end of a
/// command's output is where its errors and its exit line are, so a fold that
/// kept only the head would hide the lines a reader came for. Same number and
/// same split, so a result folds the same way in both front ends.
///
/// Lines the tool wrote, not rows the terminal draws. Counted in rows, the
/// same result would fold differently at every width, the count would name a
/// number the tool never produced, and the cut would land inside a line -
/// and half a line, with its other half folded away, is a line nobody can
/// read back.
const CAP: usize = 16;

/// The shortest reading either half of the pace is worth reporting from: a
/// tenth of a second, which is the resolution [`duration`] prints at. Under
/// it, `0.0s` reads as a measurement rather than as a wait too short to have
/// one.
const FLOOR: u64 = 100;

/// What a reader has to remember between events: the tool call still waiting
/// for its result, and what the turn has spent so far.
#[derive(Default)]
pub struct Reader {
    /// Whether the reasoning of a message is printed in full. Folded to its
    /// first line otherwise, which is what a reader of the answer wants.
    think: bool,
    /// Whether a tool's result is printed whole. Capped at [`CAP`] otherwise,
    /// so one long result cannot push the answer it led to off the screen.
    whole: bool,
    open_call: Option<String>,
    /// Tokens billed by every step of the turn in progress. `None` until a
    /// message carries usage, because a build that does not measure tokens
    /// must not be reported as a turn that spent none.
    spent: Option<Usage>,
    /// When the turn in progress started, from the journal's own clock.
    started: Option<u64>,
    /// How fast the model answered, folded over the steps that recorded it.
    pace: Pace,
}

/// What the journal says about the speed of a turn, as upstream's own turn
/// footer folds it: the wait for the first token, and the rate the rest of it
/// decoded at.
///
/// Both are derived here rather than carried across the boundary, because both
/// are arithmetic over event times the journal already holds - and a surface
/// deriving them cannot disagree with a journal it read.
#[derive(Debug, Default, Clone, Copy)]
struct Pace {
    /// When the step in progress started, and when its first chunk arrived.
    step: Option<u64>,
    first: Option<u64>,
    /// The wait for the first token of the turn's first step, which is the
    /// one a reader is waiting through. Later steps are waiting on a tool.
    waited: Option<u64>,
    /// Milliseconds spent decoding, and the tokens decoded in them, over every
    /// step that recorded both. A step missing either is left out rather than
    /// counted as instant.
    decoding: u64,
    decoded: u64,
}

impl Pace {
    /// A step began.
    fn step(&mut self, time: u64) {
        self.step = Some(time);
        self.first = None;
    }

    /// A chunk arrived. Only the first of a step says anything.
    fn chunk(&mut self, time: u64) {
        if self.first.is_some() {
            return;
        }
        self.first = Some(time);
        if let Some(step) = self.step {
            let waited = time.saturating_sub(step);
            // The first step's wait, and no other: what a reader waited
            // through before the answer began.
            self.waited.get_or_insert(waited);
        }
    }

    /// A message settled, carrying what it cost.
    fn settled(&mut self, time: u64, usage: Option<&Usage>) {
        let (Some(first), Some(usage)) = (self.first.take(), usage) else {
            self.step = None;
            return;
        };
        self.decoding += time.saturating_sub(first);
        self.decoded += usage.completion_tokens;
        self.step = None;
    }

    /// Tokens a second, over the steps that recorded both halves of it.
    ///
    /// `None` when too little time passed to divide by: a rate over a
    /// millisecond is not a fast model, it is an unmeasured one, and a mock
    /// that answers inside the clock's resolution would otherwise be reported
    /// as the fastest provider anyone has ever seen.
    fn rate(&self) -> Option<u64> {
        (self.decoding >= FLOOR && self.decoded > 0)
            .then(|| self.decoded * 1_000 / self.decoding)
            .filter(|rate| *rate > 0)
    }

    /// The wait for the first token, when there was one worth reporting.
    ///
    /// Under a tenth of a second `duration` prints `0.0s`, which says less
    /// than nothing: it reads as a measurement rather than as a wait too short
    /// to have one.
    fn waited(&self) -> Option<u64> {
        self.waited.filter(|waited| *waited >= FLOOR)
    }
}

impl Reader {
    /// A reader of a stream, told how much of the thinking to print.
    pub fn new(think: bool) -> Self {
        Self {
            think,
            ..Self::default()
        }
    }

    /// Print a tool's result whole, or capped.
    ///
    /// Told after the reader was built, because this is a reader changing
    /// their mind about what is already on the page: the view composes the
    /// conversation again with it. The thinking is not here for the same
    /// reason it does not need to be - a view rebuilds its composer to change
    /// that, and `think` is what it is built with.
    pub fn whole(&mut self, whole: bool) {
        self.whole = whole;
    }

    /// The lines one event produces, in order, and none for an event a
    /// finished turn does not show.
    pub fn lines(&mut self, theme: &Theme, width: usize, event: &SessionEvent) -> Vec<String> {
        match event.parse() {
            Some(known) => self.draw(theme, width, event.time, &known),
            None => vec![raw(theme, width, event)],
        }
    }

    fn draw(&mut self, theme: &Theme, width: usize, time: u64, event: &KnownEvent) -> Vec<String> {
        match event {
            KnownEvent::SessionStart { model, .. } => {
                vec![format!(
                    "session on {}",
                    theme.paint(Role::Accent, &tame(model))
                )]
            }
            KnownEvent::TurnStart { turn } => vec![
                {
                    self.spent = None;
                    self.started = Some(time);
                    self.pace = Pace::default();
                    String::new()
                },
                theme
                    .paint(Role::Heading, &format!("turn {turn}"))
                    .to_string(),
            ],
            KnownEvent::StepStart { step, .. } => {
                self.pace.step(time);
                vec![theme
                    .paint(Role::Muted, &format!("{INDENT}step {step}"))
                    .to_string()]
            }
            KnownEvent::UserMessage { content } => said(theme, width, "you", Role::Accent, content),
            KnownEvent::AssistantMessage {
                content,
                reasoning,
                usage,
                ..
            } => {
                self.pace.settled(time, usage.as_ref());
                if let Some(step) = usage {
                    // Each step is billed for the whole prompt it resent, so
                    // the turn's cost is the sum of its requests, not of its
                    // last one.
                    let spent = self.spent.get_or_insert_with(Usage::default);
                    spent.prompt_tokens += step.prompt_tokens;
                    spent.completion_tokens += step.completion_tokens;
                }
                let mut lines = match self.think {
                    true => said(theme, width, "think", Role::Muted, reasoning),
                    false => folded(theme, width, reasoning),
                };
                lines.extend(said(theme, width, "ai", Role::Topic, content));
                lines
            }
            KnownEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                self.open_call = Some(id.clone());
                let glyph = theme.glyph("▸", ">");
                vec![tool(
                    theme,
                    width,
                    glyph,
                    Role::Tool,
                    name,
                    &arguments.to_string(),
                    None,
                )]
            }
            KnownEvent::ToolResult {
                call_id,
                name,
                ok,
                content,
            } => {
                let (glyph, role) = match ok {
                    true => (theme.glyph("✓", "+"), Role::Ok),
                    false => (theme.glyph("✗", "!"), Role::Error),
                };
                // Silent when the result answers the call just made; named when
                // it does not, which is the case a reader cannot infer.
                let answers = match self.open_call.as_deref() {
                    Some(open) if open == call_id => None,
                    _ => Some(call_id.as_str()),
                };
                produced(
                    theme, width, self.whole, glyph, role, name, content, answers,
                )
            }
            KnownEvent::TurnEnd {
                turn,
                steps,
                stop_reason,
                stop_veto,
            } => {
                let dot = theme.glyph("·", "-");
                let shown = stopped(stop_reason);
                let reason = theme.paint(settled(stop_reason), &shown);
                let unit = if *steps == 1 { "step" } else { "steps" };
                let mut closing = format!("turn {turn} {dot} {reason} {dot} {steps} {unit}");
                // Under a second is not worth reporting: nobody waited for
                // it, and the figure would be the only part of an otherwise
                // repeatable turn that changed between two runs of it. The
                // threshold makes that rare, not impossible - a loaded machine
                // still crosses it - so the two cases that compare two runs
                // byte for byte drop the field rather than trust the
                // threshold. See `tests/common/mod.rs`.
                let took = self.started.take().map(|start| time.saturating_sub(start));
                if let Some(took) = took.filter(|took| *took >= 1_000) {
                    closing.push_str(&format!(" {dot} {}", duration(Duration::from_millis(took))));
                }
                if let Some(spent) = self.spent.take() {
                    let total = spent.prompt_tokens + spent.completion_tokens;
                    let noun = if total == 1 { "token" } else { "tokens" };
                    closing.push_str(&format!(" {dot} {} {noun}", tokens(total)));
                }
                // How fast it was, on the turns slow enough for that to be a
                // fact rather than a rounding: the wait for the first token,
                // and the rate the rest decoded at. Upstream's turn footer
                // carries the same pair, folded the same way - the first
                // step's wait, because a later step is waiting on a tool, and
                // a rate over the steps that recorded both halves of it.
                //
                // Behind the same threshold as the duration, and for the same
                // reason: under a second these are noise, and two runs of one
                // turn must print the same bytes.
                if took.is_some_and(|took| took >= 1_000) {
                    let pace = std::mem::take(&mut self.pace);
                    if let Some(waited) = pace.waited() {
                        closing.push_str(&format!(
                            " {dot} first token in {}",
                            duration(Duration::from_millis(waited))
                        ));
                    }
                    if let Some(rate) = pace.rate() {
                        closing.push_str(&format!(" {dot} {rate} tok/s"));
                    }
                }
                let mut lines = vec![String::new(), closing];
                if let Some(veto) = stop_veto {
                    lines.push(format!("{INDENT}held open by {}", tame(veto)));
                }
                // The one reason worth a sentence of its own. The contract
                // asks for it in as many words (§4.4.2): a surface that
                // renders a cut-off turn as an ordinary end tells the reader
                // that a sentence the model never finished is the whole reply.
                if matches!(stop_reason, StopReason::Other(reason) if reason == "max-tokens") {
                    lines.push(
                        theme
                            .paint(
                                Role::Muted,
                                &format!("{INDENT}the answer stops where the cap did; ask again to go on"),
                            )
                            .to_string(),
                    );
                }
                lines
            }
            // The streaming surface, and the frames of the turn. A finished
            // turn reads better without them - but the first chunk of a step
            // is when the model started answering, which the closing line
            // reports.
            KnownEvent::AssistantChunk { .. } => {
                self.pace.chunk(time);
                Vec::new()
            }
            KnownEvent::StepEnd { .. } => Vec::new(),
        }
    }
    /// What the turn has spent so far, over every step of it.
    ///
    /// `None` until a message carries usage: a build that does not measure
    /// tokens says nothing about them rather than saying nothing was spent.
    pub fn spent(&self) -> Option<u64> {
        self.spent
            .as_ref()
            .map(|spent| spent.prompt_tokens + spent.completion_tokens)
    }
}

/// Wall clock, as a person reads it: tenths under a minute, minutes above.
///
/// One wording for the whole binary - the footer of a running turn and the
/// closing line of a finished one - because two would drift. Minutes carry a
/// zero-padded remainder where upstream writes `1m5s`: this figure sits in a
/// footer that repaints every 80 ms, and a field that changes width under a
/// spinner reads as a glitch.
pub(super) fn duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    format!("{}m{:02}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
}

/// A token count the way upstream's conversation UI writes one: `517`,
/// `12.2K`, `1.2M`. One decimal until the figure reaches three digits, then
/// whole numbers - a turn's cost is read at a glance, not audited.
pub(super) fn tokens(count: u64) -> String {
    let scaled = |value: f64| match value >= 100.0 {
        true => format!("{}", value.round()),
        false => format!("{}", (value * 10.0).round() / 10.0),
    };
    match count {
        count if count < 1_000 => count.to_string(),
        count if count < 1_000_000 => format!("{}K", scaled(count as f64 / 1_000.0)),
        count => format!("{}M", scaled(count as f64 / 1_000_000.0)),
    }
}

/// What a whole conversation has cost and how fast it has been.
///
/// Upstream keeps the same figures on a strip beside its composer, folded over
/// the same events: how much was asked, how long the model and the tools each
/// took, how long the first token took on average, the rate the answers
/// decoded at, and what was billed. This is that fold, over a journal.
///
/// Every figure is derived rather than carried: the journal holds event times
/// and the usage a message reported, and a surface that computed them from
/// anything else would be a second answer about one conversation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub turns: u64,
    pub steps: u64,
    /// Milliseconds between a step starting and its message settling, summed.
    pub thinking: u64,
    /// Milliseconds between a tool being called and answering, summed.
    pub tooling: u64,
    /// Summed first-token waits, and how many steps recorded one.
    pub waited: u64,
    pub waits: u64,
    /// Decode time and the tokens decoded in it, over the steps recording both.
    pub decoding: u64,
    pub decoded: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Stats {
    /// The average wait for a first token, when any step recorded one.
    pub fn wait(&self) -> Option<u64> {
        (self.waits > 0).then(|| self.waited / self.waits)
    }

    /// Tokens a second while decoding, on the same terms the closing line
    /// reports it: nothing when too little time passed to divide by.
    pub fn rate(&self) -> Option<u64> {
        (self.decoding >= FLOOR && self.decoded > 0)
            .then(|| self.decoded * 1_000 / self.decoding)
            .filter(|rate| *rate > 0)
    }
}

/// Fold a journal into what it says about the conversation on it.
pub fn stats(events: &[SessionEvent]) -> Stats {
    let mut stats = Stats::default();
    let mut step: Option<u64> = None;
    let mut first: Option<u64> = None;
    // Calls waiting on a result, by the id a result pairs to (contract §4.3.1);
    // never by arrival order, because two calls in flight arrive in whichever
    // order the tools finish.
    let mut calls: Vec<(String, u64)> = Vec::new();
    for event in events {
        let time = event.time;
        match event.parse() {
            Some(KnownEvent::TurnStart { .. }) => stats.turns += 1,
            Some(KnownEvent::StepStart { .. }) => {
                stats.steps += 1;
                step = Some(time);
                first = None;
            }
            Some(KnownEvent::AssistantChunk { .. }) => {
                if first.is_none() {
                    first = Some(time);
                    if let Some(started) = step {
                        stats.waited += time.saturating_sub(started);
                        stats.waits += 1;
                    }
                }
            }
            Some(KnownEvent::AssistantMessage { usage, .. }) => {
                if let Some(started) = step.take() {
                    stats.thinking += time.saturating_sub(started);
                }
                if let (Some(first), Some(usage)) = (first.take(), usage.as_ref()) {
                    stats.decoding += time.saturating_sub(first);
                    stats.decoded += usage.completion_tokens;
                }
                if let Some(usage) = usage {
                    stats.prompt_tokens += usage.prompt_tokens;
                    stats.completion_tokens += usage.completion_tokens;
                }
            }
            Some(KnownEvent::ToolCall { id, .. }) => calls.push((id, time)),
            Some(KnownEvent::ToolResult { call_id, .. }) => {
                if let Some(at) = calls.iter().position(|(id, _)| *id == call_id) {
                    let (_, called) = calls.remove(at);
                    stats.tooling += time.saturating_sub(called);
                }
            }
            _ => {}
        }
    }
    stats
}

/// The strip a reader asks for: what was asked, what it took, how fast it was,
/// and what it cost.
///
/// Grouped the way upstream groups it, and a group with nothing in it is left
/// out whole rather than printed as zeroes - a conversation whose every
/// request failed has counts and no billing, and saying `0 tokens` would read
/// as a conversation that was free rather than one that never got an answer.
pub fn told(theme: &Theme, stats: &Stats) -> Vec<String> {
    if stats.turns == 0 && stats.steps == 0 {
        return vec![theme
            .paint(Role::Muted, "nothing has been asked yet")
            .to_string()];
    }
    let dot = theme.glyph("·", "-");
    let groups: Vec<String> = [
        Some(counted(stats, dot)),
        took(stats, dot),
        fast(stats, dot),
        billed(stats, dot),
    ]
    .into_iter()
    .flatten()
    .collect();
    vec![
        theme.paint(Role::Heading, "stats").to_string(),
        format!("  {}", theme.paint(Role::Muted, &groups.join("   "))),
    ]
}

/// How much was asked.
///
/// A turn that failed before its first step is still a turn, and each half is
/// counted where it is there rather than the pair being counted together.
fn counted(stats: &Stats, dot: &str) -> String {
    let mut counts = Vec::new();
    if stats.turns > 0 {
        counts.push(match stats.turns {
            1 => "1 turn".to_string(),
            turns => format!("{turns} turns"),
        });
    }
    if stats.steps > 0 {
        counts.push(match stats.steps {
            1 => "1 step".to_string(),
            steps => format!("{steps} steps"),
        });
    }
    counts.join(&format!(" {dot} "))
}

/// How long the model and the tools each took.
fn took(stats: &Stats, dot: &str) -> Option<String> {
    let mut took = Vec::new();
    if stats.thinking > 0 {
        took.push(format!(
            "model {}",
            duration(Duration::from_millis(stats.thinking))
        ));
    }
    if stats.tooling > 0 {
        took.push(format!(
            "tools {}",
            duration(Duration::from_millis(stats.tooling))
        ));
    }
    (!took.is_empty()).then(|| took.join(&format!(" {dot} ")))
}

/// How fast the answers came, on the readings worth reporting.
fn fast(stats: &Stats, dot: &str) -> Option<String> {
    let mut fast = Vec::new();
    if let Some(wait) = stats.wait().filter(|wait| *wait >= FLOOR) {
        fast.push(format!(
            "first token in {} on average",
            duration(Duration::from_millis(wait))
        ));
    }
    if let Some(rate) = stats.rate() {
        fast.push(format!("{rate} tok/s"));
    }
    (!fast.is_empty()).then(|| fast.join(&format!(" {dot} ")))
}

/// What was billed, when anything was.
fn billed(stats: &Stats, dot: &str) -> Option<String> {
    let spent = stats.prompt_tokens + stats.completion_tokens;
    (spent > 0).then(|| {
        format!(
            "{} in {dot} {} out",
            tokens(stats.prompt_tokens),
            tokens(stats.completion_tokens)
        )
    })
}

/// Render a whole event stream, as a reader of a finished turn sees it.
pub fn render<W: Write>(ui: &mut Ui<W>, events: &[SessionEvent], think: bool) -> io::Result<()> {
    if events.is_empty() {
        // A page with nothing on it reads exactly like a command that did
        // nothing at all. The journal is there and it holds no events, and
        // the view has to say which of the two happened.
        let empty = ui.paint(Role::Muted, "the journal is empty").to_string();
        return ui.line(&empty);
    }
    let (theme, width) = (*ui.theme(), ui.width());
    let mut reader = Reader::new(think);
    for event in events {
        for line in reader.lines(&theme, width, event) {
            ui.line(&line)?;
        }
    }
    Ok(())
}

/// Why the turn closed, in a reader's words rather than the wire's.
///
/// The contract carries the fact; how it reads is this lane's to choose, which
/// is why `StopReason` has no such method on it. A reason added after this
/// build was compiled arrives as `Other` and is shown as the engine spelled
/// it - rendering the fallback is what lets the engine add one in a minor
/// version (contract §2).
pub(super) fn stopped(reason: &StopReason) -> String {
    match reason {
        StopReason::Natural => "natural".into(),
        StopReason::PreStepRejected => "rejected before the step".into(),
        StopReason::MaxSteps => "step budget spent".into(),
        StopReason::Cancelled => "cancelled".into(),
        // The two values §4.4.2 and §4.4.3 name on the growable enum. A
        // surface that echoed the wire word would print `max-tokens` at a
        // reader, which says what the field holds rather than what happened.
        StopReason::Other(reason) if reason == "max-tokens" => "cut off at the output cap".into(),
        StopReason::Other(reason) if reason == "failed" => "failed".into(),
        StopReason::Other(reason) => tame(reason),
    }
}

/// Whether a turn ended the way it meant to.
///
/// Only one reason is: a model that stopped writing because it had finished.
/// Every other reason means the answer on the page is missing something the
/// reader cannot see is missing - the cap cut it off, a budget ran out, a
/// listener refused the step, somebody interrupted - and a closing line that
/// painted them all alike would say so in the colour of a job well done.
fn settled(reason: &StopReason) -> Role {
    match reason {
        StopReason::Natural => Role::Ok,
        _ => Role::Warn,
    }
}

/// A labelled block of text, folded to the width. Continuation lines align
/// under the first.
pub(super) fn said(theme: &Theme, width: usize, who: &str, role: Role, text: &str) -> Vec<String> {
    // The label is padded by the columns it occupies, not by the bytes it
    // takes: painted, it carries escape sequences that `{:<5}` would count.
    let label = theme.paint(role, who).to_string();
    let gap = " ".repeat(LABEL.saturating_sub(who.chars().count()));
    let pad = " ".repeat(INDENT.len() + LABEL + 1);
    let room = width.saturating_sub(pad.chars().count());

    wrap(text, room)
        .into_iter()
        .enumerate()
        .map(|(i, line)| match i {
            0 => format!("{INDENT}{label}{gap} {line}"),
            _ => format!("{pad}{line}"),
        })
        .collect()
}

/// The thinking as the one line a collapsed disclosure row shows: what the
/// model opened with, and how much more there is behind it.
///
/// The first line is the summary because that is the line upstream shows once
/// a block has finished streaming. The count is this lane's addition: a
/// terminal has no chevron to say that something is folded, so the line says
/// it in words.
fn folded(theme: &Theme, width: usize, reasoning: &str) -> Vec<String> {
    let mut said = reasoning
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(first) = said.next() else {
        return Vec::new();
    };
    let more = match said.count() {
        0 => String::new(),
        rest => format!("  {}", more(rest)),
    };
    let pad = INDENT.len() + LABEL + 1;
    let room = width.saturating_sub(pad + more.chars().count());
    let summary = truncate(first, room, theme.charset());
    let label = theme.paint(Role::Muted, "think");
    let body = format!("{summary}{more}");
    let text = theme.paint(Role::Muted, &body);
    vec![format!("{INDENT}{label} {text}")]
}

/// One tool line: a glyph, the tool's name, and a value it authored.
///
/// The call row. Its value is the arguments as JSON, flattened and cut,
/// because a reader checks arguments against what they asked for rather than
/// reading them - [`produced`] lays out the answer to them.
pub(super) fn tool(
    theme: &Theme,
    width: usize,
    glyph: &str,
    role: Role,
    name: &str,
    value: &str,
    answers: Option<&str>,
) -> String {
    let name = tame(name);
    let head = format!("{INDENT}{glyph} {name}  ");
    let room = width.saturating_sub(visible_width(&head));
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = truncate(&flat, room, theme.charset());
    let mark = theme.paint(role, glyph);
    let line = format!("{INDENT}{mark} {name}  {value}");
    match answers {
        Some(call) => format!("{line} (for {})", tame(call)),
        None => line,
    }
}

/// How much of something a fold is hiding.
///
/// One wording, because a journal folds two things - a model's thinking and a
/// tool's result - and a reader who has learnt to read one count should not
/// have to learn the other.
fn more(lines: usize) -> String {
    match lines {
        1 => "+1 line".to_string(),
        rest => format!("+{rest} lines"),
    }
}

/// What a tool call produced, folded to the width and capped in height.
///
/// [`tool`] cuts a value to one line. That is right for the arguments of a
/// call and wrong for its result: the arguments are JSON a reader recognises,
/// and the result is the work of the turn. Cut, a file the agent read was the
/// first eighty columns of its first line, and no view in this binary would
/// show the rest of it.
///
/// Folded like prose, so newlines the tool wrote are newlines on the page -
/// command output is lines, and flattening them to one paragraph loses the
/// shape a reader reads it by. Capped at [`CAP`], so one long result cannot
/// push the answer it led to off the top of the screen - unless the caller
/// asked for the whole of it, which is a reader saying they came for the
/// output rather than for the answer it led to.
#[allow(clippy::too_many_arguments)]
fn produced(
    theme: &Theme,
    width: usize,
    whole: bool,
    glyph: &str,
    role: Role,
    name: &str,
    content: &str,
    answers: Option<&str>,
) -> Vec<String> {
    let name = tame(name);
    let head = format!("{INDENT}{glyph} {name}  ");
    let says = answers.map(|call| format!(" (for {})", tame(call)));
    let says = says.unwrap_or_default();
    // The marker is folded around rather than added afterwards: a row that
    // overran the width by its length would corrupt every row drawn under it.
    let room = width
        .saturating_sub(visible_width(&head) + visible_width(&says))
        .max(1);
    // Columns to measure the room, columns to fill it: the head is what the
    // first row was given, so the rows under it start where its text does.
    let pad = " ".repeat(visible_width(&head));

    let mut said: Vec<&str> = content.lines().collect();
    let folded = (!whole)
        .then_some(said.len())
        .and_then(|lines| lines.checked_sub(CAP))
        .filter(|hidden| *hidden > 0)
        .map(|hidden| {
            let keep = CAP.div_ceil(2);
            let tail = said.split_off(said.len() - (CAP - keep));
            said.truncate(keep);
            (hidden, tail)
        });

    let rows = match folded {
        // `content` rather than the lines it was split into, so a result with
        // no newline in it - and an empty one - is folded by the one rule.
        None => wrap(content, room),
        Some((hidden, tail)) => {
            let mut rows = wrap(&said.join("\n"), room);
            let fold = format!("{} {}", theme.glyph("…", "..."), more(hidden));
            rows.push(theme.paint(Role::Muted, &fold).to_string());
            rows.extend(wrap(&tail.join("\n"), room));
            rows
        }
    };

    let last = rows.len().saturating_sub(1);
    let mark = theme.paint(role, glyph);
    rows.into_iter()
        .enumerate()
        .map(|(at, line)| {
            let says = match at == last {
                true => says.as_str(),
                false => "",
            };
            match at {
                0 => format!("{INDENT}{mark} {name}  {line}{says}"),
                _ => format!("{pad}{line}{says}"),
            }
        })
        .collect()
}

/// A type this build does not know. The contract says pass it through, so it
/// is shown rather than dropped.
fn raw(theme: &Theme, width: usize, event: &SessionEvent) -> String {
    let named = tame(&event.ty);
    let ty = theme.paint(Role::Topic, &named);
    let room = width.saturating_sub(visible_width(&named) + 4);
    let data = truncate(&event.data.to_string(), room, theme.charset());
    format!("{INDENT}{ty}  {data}")
}

/// Test Design Specification: the timeline renderer.
///
/// Features tested: the shape of a whole turn, that streaming events are
/// silent, correlation by `call_id`, a failed tool, an unknown type, the
/// folding of a model's thinking, the folding and the height cap of what a
/// tool produced, and the width rules.
///
/// Features NOT tested here: the colour policy (owned by `tetanus-ui`) and the
/// journal (owned by `tetanus-session`).
///
/// Environmental needs: none. Every case renders into a `Vec<u8>`.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    /// An event at a stated moment on the journal's clock.
    fn timed(time: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            time,
            ..event(ty, data)
        }
    }

    fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            ty: ty.into(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
        }
    }

    fn rendered(events: &[SessionEvent], charset: Charset, width: usize) -> String {
        thought(events, charset, width, false)
    }

    /// The same, with the thinking asked for or folded.
    fn thought(events: &[SessionEvent], charset: Charset, width: usize, think: bool) -> String {
        let mut ui = buffered(Theme::new(false, charset), width);
        render(&mut ui, events, think).expect("render");
        ui.contents()
    }

    /// A message that thought about it first.
    fn reasoned(reasoning: &str) -> Vec<SessionEvent> {
        vec![event(
            "assistant/message",
            json!({ "content": "42", "reasoning": reasoning }),
        )]
    }

    /// TC-CLI-TL-12: a journal with no events in it.
    /// Expected: a line saying so. A page with nothing on it reads exactly
    /// like a command that did nothing, and the two are worth telling apart -
    /// this view is the one a user reaches for when they are not sure the run
    /// happened.
    #[test]
    fn an_empty_journal_says_it_is_empty() {
        assert_eq!(
            rendered(&[], Charset::Unicode, 80),
            "the journal is empty\n"
        );
    }

    /// TC-CLI-TL-1: one whole turn.
    /// Expected: the documented shape, and nothing at all from `step/end` or
    /// the chunks the assembled message already carries.
    #[test]
    fn a_turn_reads_as_a_conversation() {
        let out = rendered(
            &[
                event("turn/start", json!({ "turn": 1 })),
                event("step/start", json!({ "turn": 1, "step": 1 })),
                event("user/message", json!({ "content": "echo this" })),
                event(
                    "assistant/chunk",
                    json!({ "chunk": "text", "delta": "on ", "turn": 1, "step": 1 }),
                ),
                event("assistant/message", json!({ "content": "on it" })),
                event(
                    "tool/call",
                    json!({ "id": "c1", "name": "echo", "arguments": { "text": "hi" } }),
                ),
                event(
                    "tool/result",
                    json!({ "call_id": "c1", "name": "echo", "ok": true, "content": "hi" }),
                ),
                event("step/end", json!({ "turn": 1, "step": 1 })),
                event(
                    "turn/end",
                    json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
                ),
            ],
            Charset::Unicode,
            80,
        );

        assert_eq!(
            out,
            "\nturn 1\n  step 1\n  you   echo this\n  ai    on it\n  \
             ▸ echo  {\"text\":\"hi\"}\n  ✓ echo  hi\n\nturn 1 · natural · 1 step\n"
        );
    }

    /// TC-CLI-TL-2: a result that does not answer the call just made.
    /// Expected: the line names the call it answers. Pairing is by `call_id`
    /// and never by arrival order (contract §4.3.1), and this is the case a
    /// reader cannot work out unaided.
    #[test]
    fn an_out_of_order_result_names_its_call() {
        let out = rendered(
            &[
                event(
                    "tool/call",
                    json!({ "id": "c1", "name": "read", "arguments": {} }),
                ),
                event(
                    "tool/call",
                    json!({ "id": "c2", "name": "list", "arguments": {} }),
                ),
                event(
                    "tool/result",
                    json!({ "call_id": "c1", "name": "read", "ok": false, "content": "denied" }),
                ),
            ],
            Charset::Unicode,
            80,
        );

        assert!(out.ends_with("  ✗ read  denied (for c1)\n"), "{out}");
    }

    /// TC-CLI-TL-3: a type this build does not know.
    /// Expected: the line is shown with its payload, not dropped. The durable
    /// vocabulary grows, and a surface that drops an unknown type hides work
    /// the agent really did.
    #[test]
    fn an_unknown_type_is_passed_through() {
        let out = rendered(
            &[event("todo/write", json!({ "items": 3 }))],
            Charset::Unicode,
            80,
        );

        assert_eq!(out, "  todo/write  {\"items\":3}\n");
    }

    /// TC-CLI-TL-4: the width rules.
    /// Expected: a value the tool authored is cut to the line, and a
    /// multi-line message aligns under its own first line.
    #[test]
    fn long_values_are_cut_and_wrapped_text_stays_aligned() {
        let out = rendered(
            &[
                event("assistant/message", json!({ "content": "one\ntwo" })),
                event(
                    "tool/call",
                    json!({ "id": "c1", "name": "echo", "arguments": { "text": "x".repeat(60) } }),
                ),
            ],
            Charset::Unicode,
            40,
        );

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "  ai    one");
        assert_eq!(lines[1], "        two");
        assert_eq!(lines[2].chars().count(), 40, "{:?}", lines[2]);
        assert!(lines[2].ends_with('…'), "{:?}", lines[2]);
    }

    /// TC-CLI-TL-5: an ASCII terminal.
    /// Expected: ASCII marks of the same width, so the columns still line up
    /// where braille and check marks cannot be drawn.
    #[test]
    fn an_ascii_terminal_keeps_the_columns() {
        let out = rendered(
            &[
                event(
                    "tool/call",
                    json!({ "id": "c1", "name": "echo", "arguments": {} }),
                ),
                event(
                    "tool/result",
                    json!({ "call_id": "c1", "name": "echo", "ok": true, "content": "hi" }),
                ),
                event(
                    "turn/end",
                    json!({ "turn": 2, "steps": 1, "stop_reason": "max-steps" }),
                ),
            ],
            Charset::Ascii,
            80,
        );

        assert!(out.is_ascii(), "{out:?}");
        assert!(out.contains("  > echo  {}\n  + echo  hi\n"), "{out:?}");
        assert!(
            out.ends_with("turn 2 - step budget spent - 1 step\n"),
            "{out}"
        );
    }
    /// TC-CLI-TL-6: a message longer than the terminal is wide.
    /// Expected: it is folded at the width, and every continuation line starts
    /// in the text column, not in column zero. Left to the terminal, a long
    /// answer stops looking like it belongs to the speaker who said it.
    #[test]
    fn a_long_message_folds_under_its_label() {
        let text = "the agent claims your prompt, assembles a prompt and a tool catalogue";
        let out = rendered(
            &[event("assistant/message", json!({ "content": text }))],
            Charset::Unicode,
            40,
        );

        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() > 1, "nothing was folded:\n{out}");
        assert!(lines[0].starts_with("  ai    the agent"), "{out}");
        for line in &lines[1..] {
            assert!(line.starts_with("        "), "`{line}` lost the column");
            assert!(line.chars().count() <= 40, "`{line}` overruns 40");
        }
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>()[1..].join(" "),
            text
        );
    }

    /// TC-CLI-TL-7: the same block with colour switched on.
    /// Expected: with the escape sequences taken out, the coloured rendering
    /// is the plain rendering, character for character. A label is painted, so
    /// it carries escapes that a width-padded format counts as characters -
    /// which would sit `ai` one column off and `you` three.
    #[test]
    fn colour_does_not_move_the_text_column() {
        let events = [
            event("user/message", json!({ "content": "echo this" })),
            event("assistant/message", json!({ "content": "on it" })),
        ];

        let mut painted = buffered(Theme::new(true, Charset::Unicode), 80);
        render(&mut painted, &events, false).expect("render");
        let painted = painted.contents();

        assert!(
            painted.contains('\u{1b}'),
            "nothing was painted:\n{painted:?}"
        );
        assert_eq!(
            unpainted(&painted),
            rendered(&events, Charset::Unicode, 80),
            "colour moved the text"
        );
    }

    /// TC-CLI-TL-8: every stop reason, including one added after this build.
    /// Expected: each known reason reads as this lane words it, and an
    /// unknown one is shown exactly as the engine spelled it rather than
    /// dropped or reported as an error. Rendering the `Other` fallback is what
    /// lets the engine add a reason in a minor version (contract §2).
    #[test]
    fn a_stop_reason_this_build_never_heard_of_is_still_shown() {
        for (wire, shown) in [
            ("natural", "natural"),
            ("pre-step-rejected", "rejected before the step"),
            ("max-steps", "step budget spent"),
            ("cancelled", "cancelled"),
            ("budget-exhausted", "budget-exhausted"),
        ] {
            let out = rendered(
                &[event(
                    "turn/end",
                    json!({ "turn": 1, "steps": 2, "stop_reason": wire }),
                )],
                Charset::Unicode,
                80,
            );

            assert_eq!(out, format!("\nturn 1 · {shown} · 2 steps\n"), "{wire}");
        }
    }

    /// The same line as a terminal would show it, with the SGR sequences the
    /// theme wrote taken back out.
    fn unpainted(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(char) = chars.next() {
            if char != '\u{1b}' {
                out.push(char);
                continue;
            }
            for escape in chars.by_ref() {
                if escape == 'm' {
                    break;
                }
            }
        }
        out
    }

    /// TC-CLI-TL-9: the composer and the writer agree.
    /// Expected: the lines `Reader` hands back are exactly the bytes `render`
    /// writes. The live view builds its frames from this same reader, so the
    /// day the two disagree is the day one turn is worded two ways.
    #[test]
    fn the_composer_hands_back_what_the_writer_writes() {
        let events = [
            event("turn/start", json!({ "turn": 1 })),
            event("user/message", json!({ "content": "echo this" })),
            event(
                "assistant/chunk",
                json!({ "chunk": "text", "delta": "on ", "turn": 1, "step": 1 }),
            ),
            event("assistant/message", json!({ "content": "on it" })),
            event(
                "turn/end",
                json!({ "turn": 1, "steps": 2, "stop_reason": "natural" }),
            ),
        ];

        let theme = Theme::new(false, Charset::Unicode);
        let mut reader = Reader::default();
        let composed: Vec<String> = events
            .iter()
            .flat_map(|event| reader.lines(&theme, 80, event))
            .collect();

        assert_eq!(
            format!("{}\n", composed.join("\n")),
            rendered(&events, Charset::Unicode, 80)
        );
    }

    /// TC-CLI-TL-10: what a turn was billed.
    /// Expected: the closing line reports the sum over every step, because a
    /// step is billed for the whole prompt it resent. A turn whose messages
    /// carry no usage says nothing about tokens - a build that does not
    /// measure them must not be reported as a turn that spent none.
    #[test]
    fn the_closing_line_reports_what_the_turn_spent() {
        let mut events = vec![
            event("turn/start", json!({ "turn": 1 })),
            event(
                "assistant/message",
                json!({ "content": "one", "usage": { "prompt_tokens": 20, "completion_tokens": 5 } }),
            ),
            event(
                "assistant/message",
                json!({ "content": "two", "usage": { "prompt_tokens": 28, "completion_tokens": 4 } }),
            ),
            event(
                "turn/end",
                json!({ "turn": 1, "steps": 2, "stop_reason": "natural" }),
            ),
        ];
        let told = rendered(&events, Charset::Unicode, 80);
        assert!(
            told.ends_with("turn 1 \u{b7} natural \u{b7} 2 steps \u{b7} 57 tokens\n"),
            "{told}"
        );

        events[1] = event("assistant/message", json!({ "content": "one" }));
        events[2] = event("assistant/message", json!({ "content": "two" }));
        let silent = rendered(&events, Charset::Unicode, 80);
        assert!(
            silent.ends_with("turn 1 \u{b7} natural \u{b7} 2 steps\n"),
            "{silent}"
        );
    }

    /// TC-CLI-TL-11: a second turn in the same journal.
    /// Expected: each closing line reports its own turn. A tally that carried
    /// over would make every turn after the first look more expensive than it
    /// was, and a resumed session is the normal case, not the odd one.
    #[test]
    fn each_turn_is_billed_for_itself() {
        let mut events = Vec::new();
        for turn in 1..=2 {
            events.push(event("turn/start", json!({ "turn": turn })));
            events.push(event(
                "assistant/message",
                json!({ "content": "hi", "usage": { "prompt_tokens": 10, "completion_tokens": 2 } }),
            ));
            events.push(event(
                "turn/end",
                json!({ "turn": turn, "steps": 1, "stop_reason": "natural" }),
            ));
        }
        let told = rendered(&events, Charset::Unicode, 80);
        assert_eq!(
            told.matches("\u{b7} 12 tokens").count(),
            2,
            "a tally carried over:\n{told}"
        );
        assert!(
            told.contains("turn 2 \u{b7} natural \u{b7} 1 step \u{b7} 12 tokens"),
            "{told}"
        );
    }

    /// TC-CLI-TL-13: how long the turn took.
    /// Expected: a turn whose journal spans twelve seconds says so, in the
    /// same wording the live footer uses; a turn under a second says nothing,
    /// because a sub-second figure tells a reader nothing they waited for.
    /// This case fixes the journal's timestamps, so it is the one that asserts
    /// the field; TC-CLI-2 and TC-CLI-UI-4 compare two real runs and drop it.
    #[test]
    fn a_turn_that_took_time_says_how_long() {
        let slow = [
            timed(0, "turn/start", json!({ "turn": 1 })),
            timed(
                12_400,
                "turn/end",
                json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
            ),
        ];
        let told = rendered(&slow, Charset::Unicode, 80);
        assert!(
            told.ends_with("turn 1 \u{b7} natural \u{b7} 1 step \u{b7} 12.4s\n"),
            "{told}"
        );

        let quick = [
            timed(0, "turn/start", json!({ "turn": 1 })),
            timed(
                999,
                "turn/end",
                json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
            ),
        ];
        let told = rendered(&quick, Charset::Unicode, 80);
        assert!(
            told.ends_with("turn 1 \u{b7} natural \u{b7} 1 step\n"),
            "{told}"
        );
    }

    /// TC-CLI-TL-14: the wording of a duration, at both scales.
    /// Expected: tenths of a second under a minute, and minutes with a padded
    /// remainder above it - the same string the live footer shows, because
    /// there is one function and both callers use it.
    #[test]
    fn a_duration_is_written_one_way_for_the_whole_binary() {
        for (millis, shown) in [
            (0, "0.0s"),
            (1_500, "1.5s"),
            (59_940, "59.9s"),
            (60_000, "1m00s"),
            (65_000, "1m05s"),
            (3_725_000, "62m05s"),
        ] {
            assert_eq!(duration(Duration::from_millis(millis)), shown, "{millis}");
        }
    }

    /// TC-CLI-TL-26: the compact figure, at every scale.
    /// Expected: upstream's own rule - plain under a thousand, one decimal
    /// until the figure reaches three digits, then whole numbers. A turn's
    /// cost is read at a glance; `1234567 tokens` is not read at all.
    #[test]
    fn a_token_count_is_written_the_way_upstream_writes_one() {
        for (count, shown) in [
            (0, "0"),
            (1, "1"),
            (999, "999"),
            (1_000, "1K"),
            (1_050, "1.1K"),
            (12_150, "12.2K"),
            (517_000, "517K"),
            (999_999, "1000K"),
            (1_234_567, "1.2M"),
            (150_000_000, "150M"),
        ] {
            assert_eq!(tokens(count), shown, "{count}");
        }
    }

    /// TC-CLI-TL-15: a long think, with nothing asked for.
    /// Expected: one line - the first thing the model said to itself, and a
    /// count of what is folded behind it. A reasoning model spends more lines
    /// deciding than answering, and a transcript that prints all of them has
    /// buried the answer it was written to show.
    #[test]
    fn thinking_is_folded_to_its_first_line() {
        let out = thought(
            &reasoned("Work it out.\nSix by seven.\nThat is 42."),
            Charset::Unicode,
            80,
            false,
        );

        assert_eq!(out, "  think Work it out.  +2 lines\n  ai    42\n");
    }

    /// TC-CLI-TL-16: the same think, asked for.
    /// Expected: every line of it, under the same label and aligned under the
    /// first, with the model's own line breaks kept. `--think` is the
    /// disclosure row upstream opens on a click, and opening it must not
    /// reword what it opens.
    #[test]
    fn think_prints_the_whole_of_it() {
        let out = thought(
            &reasoned("Work it out.\nSix by seven.\nThat is 42."),
            Charset::Unicode,
            80,
            true,
        );

        assert_eq!(
            out,
            "  think Work it out.\n        Six by seven.\n        That is 42.\n  ai    42\n"
        );
    }

    /// TC-CLI-TL-17: a think of exactly one line, and a message with none.
    /// Expected: no count on the first - there is nothing behind it to count -
    /// and not a line of any kind on the second. A label naming an empty
    /// thought is a lie about what the model did.
    #[test]
    fn a_fold_counts_only_what_it_hides() {
        let one = thought(&reasoned("Six by seven."), Charset::Unicode, 80, false);
        assert_eq!(one, "  think Six by seven.\n  ai    42\n");

        let none = thought(&reasoned(""), Charset::Unicode, 80, false);
        assert_eq!(none, "  ai    42\n");
    }

    /// TC-CLI-TL-18: a folded think too wide for the terminal.
    /// Expected: the summary gives way, the count does not. The count is the
    /// part a reader cannot guess, and a line that wraps has stopped being a
    /// fold.
    #[test]
    fn a_fold_is_one_line_however_narrow_the_terminal() {
        let out = thought(
            &reasoned("A first line with a great many words in it indeed.\nAnd a second."),
            Charset::Unicode,
            40,
            false,
        );
        let folded = out.lines().next().expect("a line");

        assert_eq!(folded.chars().count(), 40);
        assert!(
            folded.ends_with("+1 line"),
            "the count went missing: {folded:?}"
        );
        assert!(folded.contains('\u{2026}'), "nothing was cut: {folded:?}");
    }

    /// A result of `lines` numbered rows, each one a line a wide terminal
    /// draws whole and a narrow one folds.
    fn produced(lines: usize) -> Vec<SessionEvent> {
        let content: Vec<String> = (1..=lines)
            .map(|at| format!("row {at} of the output that a tool produced"))
            .collect();
        vec![
            event(
                "tool/call",
                json!({ "id": "c1", "name": "read", "arguments": {} }),
            ),
            event(
                "tool/result",
                json!({
                    "call_id": "c1",
                    "name": "read",
                    "ok": true,
                    "content": content.join("\n"),
                }),
            ),
        ]
    }

    /// TC-CLI-TL-19: a result wider than the terminal.
    /// Expected: folded under the value column rather than cut, with every
    /// word kept and no ellipsis. A tool's result is the work of the turn, and
    /// a reader who can see only its first line has to leave the journal to
    /// find out what the agent actually got back.
    #[test]
    fn a_result_too_wide_for_the_line_is_folded_and_not_cut() {
        let words = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let out = rendered(
            &[
                event(
                    "tool/call",
                    json!({ "id": "c1", "name": "read", "arguments": {} }),
                ),
                event(
                    "tool/result",
                    json!({ "call_id": "c1", "name": "read", "ok": true, "content": words }),
                ),
            ],
            Charset::Unicode,
            40,
        );

        let lines: Vec<&str> = out.lines().skip(1).collect();
        assert!(lines.len() > 1, "not folded: {lines:?}");
        for line in &lines {
            assert!(line.chars().count() <= 40, "`{line}` overruns 40");
        }
        assert!(!out.contains('\u{2026}'), "something was cut: {out:?}");
        // Every word, and the continuations under the first one.
        let whole = lines.join(" ");
        let said: Vec<&str> = whole.split_whitespace().skip(2).collect();
        assert_eq!(said.join(" "), words, "{lines:?}");
        // Columns, not bytes: the mark in front of the name is one column and
        // three bytes, which is the whole reason the padding is built the way
        // it is.
        let column = lines[0]
            .split("alpha")
            .next()
            .expect("the value starts somewhere")
            .chars()
            .count();
        for line in &lines[1..] {
            let indent = line.chars().count() - line.trim_start().chars().count();
            assert_eq!(indent, column, "{lines:?}");
        }
    }

    /// TC-CLI-TL-20: a result longer than the cap.
    /// Expected: the first eight lines, a count of what is folded away, and
    /// the last eight - upstream's split, so the same result folds the same
    /// way in both front ends. The tail is kept because that is where a
    /// command puts its errors and its exit line.
    #[test]
    fn a_result_longer_than_the_cap_keeps_its_head_and_its_tail() {
        let out = rendered(&produced(40), Charset::Unicode, 80);
        let rows: Vec<&str> = out.lines().skip(1).collect();

        assert_eq!(rows.len(), CAP + 1, "{rows:?}");
        assert!(rows[0].contains("row 1 of"), "{rows:?}");
        assert!(rows[7].contains("row 8 of"), "{rows:?}");
        assert_eq!(rows[8].trim(), "\u{2026} +24 lines", "{rows:?}");
        assert!(rows[9].contains("row 33 of"), "{rows:?}");
        assert!(rows[CAP].contains("row 40 of"), "{rows:?}");
    }

    /// TC-CLI-TL-21: the same result at a width that folds every line of it.
    /// Expected: the count is the same number, and the same eight lines are
    /// kept either end. The cap counts what the tool wrote, so a reader who
    /// resizes the terminal is not told a different story about the same
    /// journal - and no cut lands inside a line, which would fold away half of
    /// a line and leave the other half unreadable.
    #[test]
    fn the_count_is_of_the_lines_the_tool_wrote_not_the_ones_drawn() {
        let wide = rendered(&produced(40), Charset::Unicode, 80);
        let narrow = rendered(&produced(40), Charset::Unicode, 30);

        assert!(narrow.lines().count() > wide.lines().count(), "{narrow}");
        for out in [&wide, &narrow] {
            assert!(out.contains("+24 lines"), "{out}");
            assert!(out.contains("row 8 of"), "{out}");
            assert!(!out.contains("row 9 of"), "{out}");
            assert!(out.contains("row 33 of"), "{out}");
            assert!(!out.contains("row 32 of"), "{out}");
        }
    }

    /// TC-CLI-TL-22: a result exactly the length of the cap, and one row over.
    /// Expected: nothing is folded at the cap, and one row over folds with a
    /// count of one. An off-by-one here reads as a view that hides a row and
    /// says nothing about it.
    #[test]
    fn the_cap_folds_nothing_until_it_is_passed() {
        let flat = rendered(&produced(CAP), Charset::Unicode, 80);
        assert_eq!(flat.lines().skip(1).count(), CAP, "{flat}");
        assert!(flat.contains("row 8 of"), "{flat}");
        assert!(!flat.contains('\u{2026}'), "{flat}");

        let over = rendered(&produced(CAP + 1), Charset::Unicode, 80);
        assert!(over.contains("+1 line\n"), "{over}");
        assert!(!over.contains("+1 lines"), "{over}");
    }

    /// TC-CLI-TL-23: a folded result that answers a call out of order.
    /// Expected: the marker is on the last row, and no row overruns the width.
    /// The marker is the one part of the line a reader cannot work out, so it
    /// is folded around rather than added to a row that was already full.
    #[test]
    fn a_folded_result_names_its_call_without_overrunning() {
        let out = rendered(
            &[
                event(
                    "tool/call",
                    json!({ "id": "c1", "name": "read", "arguments": {} }),
                ),
                event(
                    "tool/call",
                    json!({ "id": "c2", "name": "list", "arguments": {} }),
                ),
                event(
                    "tool/result",
                    json!({
                        "call_id": "c1",
                        "name": "read",
                        "ok": true,
                        "content": "alpha bravo charlie delta echo foxtrot golf hotel india",
                    }),
                ),
            ],
            Charset::Unicode,
            40,
        );

        let lines: Vec<&str> = out.lines().collect();
        for line in &lines {
            assert!(line.chars().count() <= 40, "`{line}` overruns 40");
        }
        assert!(out.ends_with("(for c1)\n"), "{out}");
        assert_eq!(
            out.matches("(for c1)").count(),
            1,
            "said on more than one row: {out}"
        );
    }

    /// TC-CLI-TL-24: a journal whose short values carry escape sequences.
    /// Expected: not one of them reaches the page, in any of the six places a
    /// value arrives as itself rather than as prose - the model, an unknown
    /// event type, a tool's name, the `call_id` a late result names, the stop
    /// reason and the veto that held the turn open - and the words around
    /// each of them are still drawn. A journal is a file, and a file is
    /// something a reader can be sent.
    #[test]
    fn a_short_value_out_of_a_journal_cannot_drive_the_terminal() {
        let clear = "\u{1b}[2J";
        let told = rendered(
            &[
                event(
                    "session/start",
                    json!({ "session_id": "s", "provider": "mock", "max_steps": 4,
                            "model": format!("deep{clear}seek") }),
                ),
                event(&format!("weather/{clear}report"), json!({ "wind": 12 })),
                event(
                    "tool/call",
                    json!({ "id": format!("c{clear}1"), "name": format!("ec{clear}ho"),
                            "arguments": { "text": "hi" } }),
                ),
                event(
                    "tool/call",
                    json!({ "id": "c2", "name": "read", "arguments": {} }),
                ),
                event(
                    "tool/result",
                    json!({ "call_id": format!("c{clear}1"), "name": format!("ec{clear}ho"),
                            "ok": true, "content": "hi" }),
                ),
                event(
                    "turn/end",
                    json!({ "turn": 1, "steps": 1,
                            "stop_reason": format!("a {clear} veto"),
                            "stop_veto": format!("we{clear}ather") }),
                ),
            ],
            Charset::Unicode,
            80,
        );

        assert!(
            !told.contains('\u{1b}'),
            "an escape reached the page:\n{told}"
        );
        for word in [
            "deepseek",
            "weather/report",
            "echo",
            "(for c1)",
            "a  veto",
            "held open by weather",
        ] {
            assert!(told.contains(word), "`{word}` is not drawn:\n{told}");
        }
    }

    /// TC-CLI-TL-26: a turn slow enough to have a pace, over two steps.
    /// Expected: the wait for the first token of the first step, and the rate
    /// the answer decoded at, folded over the steps that recorded both halves
    /// of it. Upstream's own turn footer carries this pair, and folds it the
    /// same way: a later step's first token is a wait on a tool rather than on
    /// the model, and a step with no usage is left out of the rate rather than
    /// counted as free.
    #[test]
    fn a_slow_turn_says_how_fast_the_model_was() {
        let out = rendered(
            &[
                timed(0, "turn/start", json!({ "turn": 1 })),
                timed(0, "step/start", json!({ "turn": 1, "step": 1 })),
                // Six hundred milliseconds to the first token, then four
                // hundred to decode two hundred of them: five hundred a second.
                timed(
                    600,
                    "assistant/chunk",
                    json!({ "chunk": "text", "delta": "on ", "turn": 1, "step": 1 }),
                ),
                timed(
                    1_000,
                    "assistant/message",
                    json!({ "content": "on it", "usage": { "prompt_tokens": 10, "completion_tokens": 200 } }),
                ),
                timed(1_000, "step/start", json!({ "turn": 1, "step": 2 })),
                // A second step waits on a tool, not on the model: its wait is
                // not the one reported, and its decoding still counts.
                timed(
                    3_000,
                    "assistant/chunk",
                    json!({ "chunk": "text", "delta": "done", "turn": 1, "step": 2 }),
                ),
                timed(
                    3_600,
                    "assistant/message",
                    json!({ "content": "done", "usage": { "prompt_tokens": 10, "completion_tokens": 100 } }),
                ),
                timed(
                    4_000,
                    "turn/end",
                    json!({ "turn": 1, "steps": 2, "stop_reason": "natural" }),
                ),
            ],
            Charset::Unicode,
            120,
        );

        let closing = out.lines().last().expect("a closing line").to_string();
        assert!(closing.contains("first token in 0.6s"), "{closing}");
        // Three hundred tokens over a second of decoding.
        assert!(closing.contains("300 tok/s"), "{closing}");
        assert!(
            closing.contains("4.0s"),
            "the duration went missing: {closing}"
        );
    }

    /// TC-CLI-TL-27: the turns too fast, or too unmeasured, to have one.
    /// Expected: nothing about pace. A turn under a second is noise - two runs
    /// of it must print the same bytes - a first token inside a tenth of a
    /// second reads as `0.0s`, which is a measurement nobody made, and a
    /// message carrying no usage leaves a rate with nothing to divide.
    #[test]
    fn a_fast_or_unmeasured_turn_says_nothing_about_pace() {
        let quick = rendered(
            &[
                timed(0, "turn/start", json!({ "turn": 1 })),
                timed(0, "step/start", json!({ "turn": 1, "step": 1 })),
                timed(
                    10,
                    "assistant/chunk",
                    json!({ "chunk": "text", "delta": "on", "turn": 1, "step": 1 }),
                ),
                timed(
                    20,
                    "assistant/message",
                    json!({ "content": "on it", "usage": { "prompt_tokens": 1, "completion_tokens": 2 } }),
                ),
                timed(
                    30,
                    "turn/end",
                    json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
                ),
            ],
            Charset::Unicode,
            120,
        );
        assert!(!quick.contains("tok/s"), "{quick}");
        assert!(!quick.contains("first token"), "{quick}");

        let unmeasured = rendered(
            &[
                timed(0, "turn/start", json!({ "turn": 1 })),
                timed(0, "step/start", json!({ "turn": 1, "step": 1 })),
                timed(
                    900,
                    "assistant/chunk",
                    json!({ "chunk": "text", "delta": "on", "turn": 1, "step": 1 }),
                ),
                timed(2_000, "assistant/message", json!({ "content": "on it" })),
                timed(
                    2_000,
                    "turn/end",
                    json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
                ),
            ],
            Charset::Unicode,
            120,
        );
        // The wait is a fact the journal holds even when nothing was billed.
        assert!(unmeasured.contains("first token in 0.9s"), "{unmeasured}");
        assert!(!unmeasured.contains("tok/s"), "{unmeasured}");
    }

    /// TC-CLI-TL-28: the fold over a conversation of two turns, one of them
    /// with two tools in flight at once.
    /// Expected: every figure the strip reports, and a tool's time taken from
    /// the call its result names rather than from the call before it - two
    /// calls in flight arrive in whichever order the tools finish, which is
    /// the reason the contract pairs them by id (§4.3.1).
    #[test]
    fn the_fold_reads_a_conversation_off_its_journal() {
        let told = stats(&[
            timed(0, "turn/start", json!({ "turn": 1 })),
            timed(0, "step/start", json!({ "turn": 1, "step": 1 })),
            timed(
                500,
                "assistant/chunk",
                json!({ "chunk": "text", "delta": "on", "turn": 1, "step": 1 }),
            ),
            timed(
                1_500,
                "assistant/message",
                json!({ "content": "on it", "usage": { "prompt_tokens": 100, "completion_tokens": 50 } }),
            ),
            timed(
                1_500,
                "tool/call",
                json!({ "id": "slow", "name": "echo", "arguments": {} }),
            ),
            timed(
                1_600,
                "tool/call",
                json!({ "id": "quick", "name": "echo", "arguments": {} }),
            ),
            // The second call answers first: paired by id, the slow one is a
            // second and the quick one is a tenth.
            timed(
                1_700,
                "tool/result",
                json!({ "call_id": "quick", "name": "echo", "ok": true, "content": "" }),
            ),
            timed(
                2_500,
                "tool/result",
                json!({ "call_id": "slow", "name": "echo", "ok": true, "content": "" }),
            ),
            timed(
                2_500,
                "turn/end",
                json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
            ),
            timed(3_000, "turn/start", json!({ "turn": 2 })),
            timed(3_000, "step/start", json!({ "turn": 2, "step": 1 })),
            timed(
                3_200,
                "assistant/chunk",
                json!({ "chunk": "text", "delta": "again", "turn": 2, "step": 1 }),
            ),
            timed(
                3_700,
                "assistant/message",
                json!({ "content": "again", "usage": { "prompt_tokens": 40, "completion_tokens": 10 } }),
            ),
            timed(
                3_700,
                "turn/end",
                json!({ "turn": 2, "steps": 1, "stop_reason": "natural" }),
            ),
        ]);

        assert_eq!(told.turns, 2);
        assert_eq!(told.steps, 2);
        // 1.5s of the first step and 0.7s of the second.
        assert_eq!(told.thinking, 2_200);
        // A second for the slow call, a tenth for the quick one.
        assert_eq!(told.tooling, 1_100);
        assert_eq!(told.waits, 2);
        assert_eq!(told.wait(), Some(350));
        // Sixty tokens decoded over a second and a half.
        assert_eq!(told.decoded, 60);
        assert_eq!(told.decoding, 1_500);
        assert_eq!(told.rate(), Some(40));
        assert_eq!(told.prompt_tokens, 140);
        assert_eq!(told.completion_tokens, 60);
    }

    /// TC-CLI-TL-29: the strip, on a conversation with nothing to say about
    /// speed or money, and on one with nothing at all.
    /// Expected: a group with no data is left out whole rather than printed as
    /// zeroes. `0 tokens` reads as a conversation that was free; a
    /// conversation whose every request failed is one that never got an
    /// answer, and the counts say so on their own.
    #[test]
    fn a_group_with_nothing_in_it_is_left_out() {
        let theme = Theme::new(false, Charset::Unicode);
        let counted = told(
            &theme,
            &Stats {
                turns: 1,
                steps: 1,
                ..Stats::default()
            },
        );
        let strip = counted.join(" ");
        assert!(strip.contains("1 turn · 1 step"), "{strip}");
        assert!(!strip.contains("tok/s"), "{strip}");
        assert!(!strip.contains(" in "), "{strip}");

        let nothing = told(&theme, &Stats::default()).join(" ");
        assert!(nothing.contains("nothing has been asked yet"), "{nothing}");
    }

    /// TC-CLI-TL-30: a turn the provider cut off at its output cap.
    /// Expected: worded rather than echoed - `max-tokens` is what the field
    /// holds, not what happened - said in the warning colour rather than the
    /// one a finished turn gets, and followed by the sentence the contract
    /// asks for in as many words (§4.4.2): a surface that renders this as an
    /// ordinary end tells the reader that a sentence the model never finished
    /// is the whole reply.
    #[test]
    fn a_turn_cut_off_at_the_cap_says_the_answer_is_unfinished() {
        let events = [
            event("turn/start", json!({ "turn": 1 })),
            event(
                "assistant/message",
                json!({ "content": "it was the best of" }),
            ),
            event(
                "turn/end",
                json!({ "turn": 1, "steps": 1, "stop_reason": "max-tokens" }),
            ),
        ];
        let out = rendered(&events, Charset::Unicode, 80);

        assert!(out.contains("cut off at the output cap"), "{out}");
        assert!(
            !out.contains("max-tokens"),
            "the wire word reached the page: {out}"
        );
        assert!(out.contains("the answer stops where the cap did"), "{out}");

        // And it is not painted as a turn that ended well.
        let mut ui = buffered(Theme::new(true, Charset::Unicode), 80);
        render(&mut ui, &events, false).expect("render");
        let painted = ui.contents();
        let cut = painted
            .lines()
            .find(|line| line.contains("cut off"))
            .expect("the closing line");
        let natural = {
            let mut ui = buffered(Theme::new(true, Charset::Unicode), 80);
            let ended = [
                event("turn/start", json!({ "turn": 1 })),
                event(
                    "turn/end",
                    json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
                ),
            ];
            render(&mut ui, &ended, false).expect("render");
            ui.contents()
                .lines()
                .find(|line| line.contains("natural"))
                .expect("the closing line")
                .to_string()
        };
        let colour = |line: &str| {
            line.split('\u{1b}')
                .find(|part| part.starts_with('[') && part.contains('m'))
                .map(|part| part.split('m').next().unwrap_or_default().to_string())
        };
        assert_ne!(
            colour(cut),
            colour(&natural),
            "a cut-off turn is painted like a finished one"
        );
    }

    /// TC-CLI-TL-25: a tool whose name a terminal draws twice as wide.
    /// Expected: no row overruns the width. The room left for the value is
    /// measured in the columns the name is drawn in, not the characters it is
    /// made of, so a name in a wide script cannot push the value past the
    /// frame and every row under it into the wrong column.
    #[test]
    fn the_room_beside_a_name_is_measured_in_columns() {
        let out = rendered(
            &[
                event(
                    "tool/call",
                    json!({ "call_id": "c1", "name": "\u{65e5}\u{672c}\u{8a9e}",
                            "arguments": { "text": "alpha bravo charlie delta echo foxtrot" } }),
                ),
                event(
                    "tool/result",
                    json!({ "call_id": "c1", "name": "\u{65e5}\u{672c}\u{8a9e}", "ok": true,
                            "content": "alpha bravo charlie delta echo foxtrot golf hotel" }),
                ),
            ],
            Charset::Unicode,
            40,
        );

        for line in out.lines() {
            assert!(
                tetanus_ui::visible_width(line) <= 40,
                "`{line}` is {} columns",
                tetanus_ui::visible_width(line)
            );
        }
    }
}
