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

use std::io::{self, Write};

use tetanus_protocol::types::{KnownEvent, SessionEvent, StopReason, Usage};
use tetanus_ui::{truncate, wrap, Role, Theme, Ui};

/// Where a content line starts: two of indent, a five-column label, two more.
pub(super) const LABEL: usize = 5;
pub(super) const INDENT: &str = "  ";

/// What a reader has to remember between events: the tool call still waiting
/// for its result, and what the turn has spent so far.
#[derive(Default)]
pub struct Reader {
    open_call: Option<String>,
    /// Tokens billed by every step of the turn in progress. `None` until a
    /// message carries usage, because a build that does not measure tokens
    /// must not be reported as a turn that spent none.
    spent: Option<Usage>,
}

impl Reader {
    /// The lines one event produces, in order, and none for an event a
    /// finished turn does not show.
    pub fn lines(&mut self, theme: &Theme, width: usize, event: &SessionEvent) -> Vec<String> {
        match event.parse() {
            Some(known) => self.draw(theme, width, &known),
            None => vec![raw(theme, width, event)],
        }
    }

    fn draw(&mut self, theme: &Theme, width: usize, event: &KnownEvent) -> Vec<String> {
        match event {
            KnownEvent::SessionStart { model, .. } => {
                vec![format!("session on {}", theme.paint(Role::Accent, model))]
            }
            KnownEvent::TurnStart { turn } => vec![
                {
                    self.spent = None;
                    String::new()
                },
                theme
                    .paint(Role::Heading, &format!("turn {turn}"))
                    .to_string(),
            ],
            KnownEvent::StepStart { step, .. } => vec![theme
                .paint(Role::Muted, &format!("{INDENT}step {step}"))
                .to_string()],
            KnownEvent::UserMessage { content } => said(theme, width, "you", Role::Accent, content),
            KnownEvent::AssistantMessage {
                content,
                reasoning,
                usage,
                ..
            } => {
                if let Some(step) = usage {
                    // Each step is billed for the whole prompt it resent, so
                    // the turn's cost is the sum of its requests, not of its
                    // last one.
                    let spent = self.spent.get_or_insert_with(Usage::default);
                    spent.prompt_tokens += step.prompt_tokens;
                    spent.completion_tokens += step.completion_tokens;
                }
                let mut lines = match reasoning.is_empty() {
                    true => Vec::new(),
                    false => said(theme, width, "think", Role::Muted, reasoning),
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
                vec![tool(theme, width, glyph, role, name, content, answers)]
            }
            KnownEvent::TurnEnd {
                turn,
                steps,
                stop_reason,
                stop_veto,
            } => {
                let dot = theme.glyph("·", "-");
                let shown = stopped(stop_reason);
                let reason = theme.paint(Role::Ok, &shown);
                let unit = if *steps == 1 { "step" } else { "steps" };
                let mut closing = format!("turn {turn} {dot} {reason} {dot} {steps} {unit}");
                if let Some(spent) = self.spent.take() {
                    let total = spent.prompt_tokens + spent.completion_tokens;
                    let noun = if total == 1 { "token" } else { "tokens" };
                    closing.push_str(&format!(" {dot} {} {noun}", tokens(total)));
                }
                let mut lines = vec![String::new(), closing];
                if let Some(veto) = stop_veto {
                    lines.push(format!("{INDENT}held open by {veto}"));
                }
                lines
            }
            // The streaming surface, and the frames of the turn. A finished
            // turn reads better without them.
            KnownEvent::AssistantChunk { .. } | KnownEvent::StepEnd { .. } => Vec::new(),
        }
    }
}

/// A token count the way upstream's conversation UI writes one: `517`,
/// `12.2K`, `1.2M`. One decimal until the figure reaches three digits, then
/// whole numbers - a turn's cost is read at a glance, not audited.
fn tokens(count: u64) -> String {
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

/// Render a whole event stream, as a reader of a finished turn sees it.
pub fn render<W: Write>(ui: &mut Ui<W>, events: &[SessionEvent]) -> io::Result<()> {
    let (theme, width) = (*ui.theme(), ui.width());
    let mut reader = Reader::default();
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
        StopReason::Other(reason) => reason.clone(),
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

/// One tool line: a glyph, the tool's name, and a value it authored.
pub(super) fn tool(
    theme: &Theme,
    width: usize,
    glyph: &str,
    role: Role,
    name: &str,
    value: &str,
    answers: Option<&str>,
) -> String {
    let head = format!("{INDENT}{glyph} {name}  ");
    let room = width.saturating_sub(head.chars().count());
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = truncate(&flat, room, theme.charset());
    let mark = theme.paint(role, glyph);
    let line = format!("{INDENT}{mark} {name}  {value}");
    match answers {
        Some(call) => format!("{line} (for {call})"),
        None => line,
    }
}

/// A type this build does not know. The contract says pass it through, so it
/// is shown rather than dropped.
fn raw(theme: &Theme, width: usize, event: &SessionEvent) -> String {
    let ty = theme.paint(Role::Topic, &event.ty);
    let room = width.saturating_sub(event.ty.chars().count() + 4);
    let data = truncate(&event.data.to_string(), room, theme.charset());
    format!("{INDENT}{ty}  {data}")
}

/// Test Design Specification: the timeline renderer.
///
/// Features tested: the shape of a whole turn, that streaming events are
/// silent, correlation by `call_id`, a failed tool, an unknown type, and the
/// width rules. Features NOT tested here: the colour policy (owned by
/// `tetanus-ui`) and the journal (owned by `tetanus-session`).
///
/// Environmental needs: none. Every case renders into a `Vec<u8>`.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{buffered, Charset, Theme};

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

    fn rendered(events: &[SessionEvent], charset: Charset, width: usize) -> String {
        let mut ui = buffered(Theme::new(false, charset), width);
        render(&mut ui, events).expect("render");
        ui.contents()
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
        render(&mut painted, &events).expect("render");
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

    /// TC-CLI-TL-12: the compact figure, at every scale.
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
}
