//! A turn while it is still happening.
//!
//! The timeline reads a turn that is over: every line is final the moment it
//! is written. A live view has two kinds of line at once - the ones that are
//! settled, and the handful at the bottom that are still changing - and this
//! module is the state machine that tells them apart.
//!
//! - **Settled** lines come from [`super::timeline::Reader`], unchanged and in
//!   the same order. That is deliberate: the turn a user watches arrive is
//!   worded exactly like the turn they replay tomorrow, and a piped run - where
//!   the block is never drawn at all - writes the timeline byte for byte.
//! - The **block** holds what the settled lines cannot say yet: the answer as
//!   the chunks assemble it, and a footer naming what the turn is waiting on.
//!
//! Nothing here writes, and nothing here reads a clock. The caller owns the
//! `Screen`, the poll interval and the stopwatch, so every case in the suite is
//! a pure function of the events it feeds in.

use std::time::Duration;

use tetanus_protocol::types::{Chunk, KnownEvent, SessionEvent};
use tetanus_ui::{progress, Role, Theme};

use super::timeline::{duration, said, Reader};

/// Rows the block may occupy. A block taller than the terminal scrolls its own
/// top away and the next frame lands on the wrong row, so the answer shows its
/// tail: the words that just arrived are the ones being read.
const BLOCK: usize = 6;

/// The state of a turn as its events arrive.
pub struct Live {
    theme: Theme,
    width: usize,
    reader: Reader,
    /// Assistant text assembled from the chunks of the current step, dropped
    /// when the assembled message settles it.
    answer: String,
    /// Thinking-mode text of the current step, shown while it is the only
    /// thing arriving.
    reasoning: String,
    phase: String,
    tick: usize,
    over: bool,
}

impl Live {
    /// A view that has seen nothing yet. `phase` is what the turn is waiting
    /// on before its first event - a model that has not answered yet is the
    /// longest silence of a turn.
    pub fn new(theme: Theme, width: usize, phase: &str) -> Self {
        Self {
            theme,
            width,
            reader: Reader::default(),
            answer: String::new(),
            reasoning: String::new(),
            phase: phase.to_string(),
            tick: 0,
            over: false,
        }
    }

    /// Feed one event. The lines it returns are settled: the caller commits
    /// them above the block and never rewrites them.
    pub fn push(&mut self, event: &SessionEvent) -> Vec<String> {
        if let Some(KnownEvent::AssistantChunk { chunk, .. }) = event.parse() {
            match chunk {
                Chunk::Text { delta } => {
                    self.answer.push_str(&delta);
                    self.phase = "streaming the answer".into();
                }
                Chunk::Reasoning { delta } => {
                    self.reasoning.push_str(&delta);
                    self.phase = "thinking".into();
                }
                Chunk::ToolCall { call } => self.phase = format!("asked for {}", call.name),
            }
            // A chunk is the block's business only. The timeline does not draw
            // it, and neither does the settled half of this view.
            return Vec::new();
        }

        self.phase = self.phase_for(event);
        if matches!(event.parse(), Some(KnownEvent::AssistantMessage { .. })) {
            // The assembled message settles what the chunks were showing.
            self.answer.clear();
            self.reasoning.clear();
        }
        self.reader.lines(&self.theme, self.width, event)
    }

    /// Advance the spinner. Called on the caller's frame interval, not on an
    /// event, so a long silence still looks alive.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    /// The block as of now: the answer so far, then the footer. Empty once the
    /// turn is over, so the last frame leaves nothing behind.
    pub fn block(&self, elapsed: Duration) -> Vec<String> {
        if self.over {
            return Vec::new();
        }
        let mut lines = self.arriving();
        // The footer is the row that has to be there, so the answer gives up
        // its oldest rows for it.
        while lines.len() + 1 > BLOCK {
            lines.remove(0);
        }
        lines.push(self.footer(elapsed));
        lines
    }

    /// The part of the answer no settled line carries yet.
    fn arriving(&self) -> Vec<String> {
        let (who, role, text) = match (self.answer.is_empty(), self.reasoning.is_empty()) {
            (false, _) => ("ai", Role::Topic, &self.answer),
            (true, false) => ("think", Role::Muted, &self.reasoning),
            (true, true) => return Vec::new(),
        };
        said(&self.theme, self.width, who, role, text)
    }

    fn footer(&self, elapsed: Duration) -> String {
        let glyph = progress::frame(self.theme.charset(), self.tick);
        let dot = self.theme.glyph("·", "-");
        let text = format!("{} {dot} {}", self.phase, duration(elapsed));
        let spin = self.theme.paint(Role::Accent, glyph);
        let text = self.theme.paint(Role::Muted, &text);
        format!("  {spin} {text}")
    }

    /// What the turn is waiting on, now that this event has happened.
    fn phase_for(&mut self, event: &SessionEvent) -> String {
        match event.parse() {
            Some(KnownEvent::TurnStart { .. }) => "starting the turn".into(),
            Some(KnownEvent::StepStart { step, .. }) => format!("step {step}"),
            Some(KnownEvent::UserMessage { .. }) => "asking the model".into(),
            Some(KnownEvent::AssistantMessage { tool_calls, .. }) => match tool_calls.is_empty() {
                true => "closing the turn".into(),
                false => "running tools".into(),
            },
            Some(KnownEvent::ToolCall { name, .. }) => format!("running {name}"),
            Some(KnownEvent::ToolResult { .. }) => "asking the model".into(),
            Some(KnownEvent::TurnEnd { .. }) => {
                self.over = true;
                "done".into()
            }
            // A step boundary and any type this build does not know leave the
            // phase alone: guessing at one would be worse than the last true
            // thing said.
            _ => self.phase.clone(),
        }
    }
}

/// Test Design Specification: the live view.
///
/// Features tested: that the settled half is the timeline byte for byte, that
/// chunks settle nothing, what the block holds while an answer arrives, its
/// height, its footer, and that it empties when the turn ends.
///
/// Features NOT tested here: drawing (owned by `tetanus-ui`'s `screen.rs`),
/// the wording of a settled line (owned by `timeline.rs`), and the polling
/// that feeds this view (owned by `main.rs`, and covered end to end by
/// TC-CLI-UI-9).
///
/// Environmental needs: none. Every case is a pure function of the events it
/// feeds in and the duration it states.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{buffered, Charset};

    use super::*;

    fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            ty: ty.into(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
        }
    }

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    fn view(width: usize) -> Live {
        Live::new(theme(), width, "asking the model")
    }

    fn turn() -> Vec<SessionEvent> {
        vec![
            event("turn/start", json!({ "turn": 1 })),
            event("step/start", json!({ "turn": 1, "step": 1 })),
            event("user/message", json!({ "content": "echo this" })),
            event(
                "assistant/chunk",
                json!({ "chunk": "text", "delta": "on ", "turn": 1, "step": 1 }),
            ),
            event(
                "assistant/chunk",
                json!({ "chunk": "text", "delta": "it", "turn": 1, "step": 1 }),
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
        ]
    }

    /// TC-CLI-LIVE-1: the settled half of a whole turn.
    /// Expected: the lines this view commits are the bytes the timeline
    /// writes, in the same order. This is the promise the whole design rests
    /// on - a piped run never draws the block, so its output has to be the
    /// timeline, and a turn watched live has to read like the same turn
    /// replayed.
    #[test]
    fn what_settles_is_the_timeline() {
        let events = turn();
        let mut live = view(80);
        let settled: Vec<String> = events.iter().flat_map(|event| live.push(event)).collect();

        let mut ui = buffered(theme(), 80);
        super::super::timeline::render(&mut ui, &events).expect("render");

        assert_eq!(format!("{}\n", settled.join("\n")), ui.contents());
    }

    /// TC-CLI-LIVE-2: the chunks of an answer.
    /// Expected: nothing settles, and the block shows the text assembled so
    /// far under the same `ai` label the settled line will use. A chunk that
    /// settled would be printed twice, once in pieces and once assembled.
    #[test]
    fn a_chunk_settles_nothing_and_shows_in_the_block() {
        let mut live = view(80);
        for event in turn().iter().take(5) {
            live.push(event);
        }

        let block = live.block(Duration::from_millis(400));
        assert_eq!(block[0], "  ai    on it");
        assert!(block[1].contains("streaming the answer"), "{block:?}");
    }

    /// TC-CLI-LIVE-3: the assembled message that follows those chunks.
    /// Expected: the answer leaves the block as it settles, so the terminal
    /// holds one copy of it and not two.
    #[test]
    fn the_assembled_message_takes_the_answer_out_of_the_block() {
        let mut live = view(80);
        for event in turn().iter().take(6) {
            live.push(event);
        }

        assert_eq!(
            live.block(Duration::ZERO).len(),
            1,
            "only the footer is left"
        );
    }

    /// TC-CLI-LIVE-4: an answer longer than the block.
    /// Expected: the block stays within its height and keeps the tail. The
    /// words that just arrived are the ones being read, and a block taller
    /// than the terminal would scroll its own top away and put every later
    /// frame on the wrong row.
    #[test]
    fn a_long_answer_shows_its_tail_and_keeps_the_block_short() {
        let mut live = view(40);
        let words: String = std::iter::repeat_n("word", 60)
            .collect::<Vec<_>>()
            .join(" ");
        live.push(&event(
            "assistant/chunk",
            json!({ "chunk": "text", "delta": words, "turn": 1, "step": 1 }),
        ));

        let block = live.block(Duration::ZERO);
        assert_eq!(block.len(), BLOCK);
        assert!(
            !block[0].contains("ai"),
            "the head was kept: {:?}",
            block[0]
        );
        assert!(block[BLOCK - 2].ends_with("word"), "{block:?}");
    }

    /// TC-CLI-LIVE-5: the footer.
    /// Expected: the spinner glyph, what the turn is waiting on, and how long
    /// it has been waiting; and the glyph advances on a tick, because a turn
    /// that is silent for ten seconds still has to look alive.
    #[test]
    fn the_footer_says_what_is_being_waited_on() {
        let mut live = view(80);
        live.push(&event(
            "tool/call",
            json!({ "id": "c1", "name": "echo", "arguments": {} }),
        ));

        let first = live
            .block(Duration::from_millis(1500))
            .pop()
            .expect("footer");
        assert_eq!(first, "  ⠋ running echo · 1.5s");

        live.tick();
        let second = live.block(Duration::from_secs(75)).pop().expect("footer");
        assert_eq!(second, "  ⠙ running echo · 1m15s");
    }

    /// TC-CLI-LIVE-6: the end of the turn.
    /// Expected: the block is empty, so the last frame erases itself and the
    /// summary the timeline settled is the last word on screen.
    #[test]
    fn the_block_empties_when_the_turn_ends() {
        let mut live = view(80);
        for event in turn() {
            live.push(&event);
        }

        assert!(live.block(Duration::ZERO).is_empty());
    }

    /// TC-CLI-LIVE-7: thinking-mode text with no answer yet.
    /// Expected: it is shown under `think`, the label the timeline gives it,
    /// rather than shown as the answer or hidden until the turn is over.
    #[test]
    fn reasoning_is_shown_as_thinking() {
        let mut live = view(80);
        live.push(&event(
            "assistant/chunk",
            json!({ "chunk": "reasoning", "delta": "the user wants an echo", "turn": 1, "step": 1 }),
        ));

        let block = live.block(Duration::ZERO);
        assert_eq!(block[0], "  think the user wants an echo");
        assert!(block[1].contains("thinking"), "{block:?}");
    }
}
