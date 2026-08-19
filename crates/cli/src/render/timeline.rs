//! A turn's events as something a person reads.
//!
//! The input is a stream of `tetanus-protocol` events; the output is lines.
//! Nothing here knows where the events came from, so the same renderer serves
//! a journal on disk, an in-process turn, and a WebSocket subscription.
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

use tetanus_protocol::types::{KnownEvent, SessionEvent, StopReason};
use tetanus_ui::{truncate, wrap, Role, Ui};

/// Where a content line starts: two of indent, a five-column label, two more.
const LABEL: usize = 5;
const INDENT: &str = "  ";

/// Render a whole event stream.
pub fn render<W: Write>(ui: &mut Ui<W>, events: &[SessionEvent]) -> io::Result<()> {
    let mut open_call: Option<String> = None;
    for event in events {
        match event.parse() {
            Some(known) => draw(ui, &known, &mut open_call)?,
            None => raw(ui, event)?,
        }
    }
    Ok(())
}

fn draw<W: Write>(
    ui: &mut Ui<W>,
    event: &KnownEvent,
    open_call: &mut Option<String>,
) -> io::Result<()> {
    match event {
        KnownEvent::SessionStart { model, .. } => {
            let model = ui.paint(Role::Accent, model).to_string();
            ui.line(&format!("session on {model}"))
        }
        KnownEvent::TurnStart { turn } => ui.heading(&format!("turn {turn}")),
        KnownEvent::StepStart { step, .. } => {
            let step = ui
                .paint(Role::Muted, &format!("{INDENT}step {step}"))
                .to_string();
            ui.line(&step)
        }
        KnownEvent::UserMessage { content } => said(ui, "you", Role::Accent, content),
        KnownEvent::AssistantMessage {
            content, reasoning, ..
        } => {
            if !reasoning.is_empty() {
                said(ui, "think", Role::Muted, reasoning)?;
            }
            said(ui, "ai", Role::Topic, content)
        }
        KnownEvent::ToolCall {
            id,
            name,
            arguments,
        } => {
            *open_call = Some(id.clone());
            let glyph = ui.theme().glyph("▸", ">");
            tool(ui, glyph, Role::Tool, name, &arguments.to_string(), None)
        }
        KnownEvent::ToolResult {
            call_id,
            name,
            ok,
            content,
        } => {
            let (glyph, role) = match ok {
                true => (ui.theme().glyph("✓", "+"), Role::Ok),
                false => (ui.theme().glyph("✗", "!"), Role::Error),
            };
            // Silent when the result answers the call just made; named when it
            // does not, which is the case a reader cannot infer.
            let answers = match open_call.as_deref() {
                Some(open) if open == call_id => None,
                _ => Some(call_id.as_str()),
            };
            tool(ui, glyph, role, name, content, answers)
        }
        KnownEvent::TurnEnd {
            turn,
            steps,
            stop_reason,
            stop_veto,
        } => {
            let dot = ui.theme().glyph("·", "-");
            let reason = ui.paint(Role::Ok, &stopped(stop_reason)).to_string();
            ui.blank()?;
            let unit = if *steps == 1 { "step" } else { "steps" };
            ui.line(&format!("turn {turn} {dot} {reason} {dot} {steps} {unit}"))?;
            match stop_veto {
                Some(veto) => ui.line(&format!("{INDENT}held open by {veto}")),
                None => Ok(()),
            }
        }
        // The streaming surface, and the frames of the turn. A finished turn
        // reads better without them.
        KnownEvent::AssistantChunk { .. } | KnownEvent::StepEnd { .. } => Ok(()),
    }
}

/// Why the turn closed, in a reader's words rather than the wire's.
///
/// The contract carries the fact; how it reads is this lane's to choose, which
/// is why `StopReason` has no such method on it. A reason added after this
/// build was compiled arrives as `Other` and is shown as the engine spelled
/// it - rendering the fallback is what lets the engine add one in a minor
/// version (contract §2).
fn stopped(reason: &StopReason) -> String {
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
fn said<W: Write>(ui: &mut Ui<W>, who: &str, role: Role, text: &str) -> io::Result<()> {
    // The label is padded by the columns it occupies, not by the bytes it
    // takes: painted, it carries escape sequences that `{:<5}` would count.
    let label = ui.paint(role, who).to_string();
    let gap = " ".repeat(LABEL.saturating_sub(who.chars().count()));
    let pad = " ".repeat(INDENT.len() + LABEL + 1);
    let room = ui.width().saturating_sub(pad.chars().count());

    for (i, line) in wrap(text, room).into_iter().enumerate() {
        match i {
            0 => ui.line(&format!("{INDENT}{label}{gap} {line}"))?,
            _ => ui.line(&format!("{pad}{line}"))?,
        }
    }
    Ok(())
}

/// One tool line: a glyph, the tool's name, and a value it authored.
fn tool<W: Write>(
    ui: &mut Ui<W>,
    glyph: &str,
    role: Role,
    name: &str,
    value: &str,
    answers: Option<&str>,
) -> io::Result<()> {
    let head = format!("{INDENT}{glyph} {name}  ");
    let room = ui.width().saturating_sub(head.chars().count());
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = truncate(&flat, room, ui.theme().charset());
    let mark = ui.paint(role, glyph).to_string();
    let line = format!("{INDENT}{mark} {name}  {value}");
    match answers {
        Some(call) => ui.line(&format!("{line} (for {call})")),
        None => ui.line(&line),
    }
}

/// A type this build does not know. The contract says pass it through, so it
/// is shown rather than dropped.
fn raw<W: Write>(ui: &mut Ui<W>, event: &SessionEvent) -> io::Result<()> {
    let ty = ui.paint(Role::Topic, &event.ty).to_string();
    let room = ui.width().saturating_sub(event.ty.chars().count() + 4);
    let data = truncate(&event.data.to_string(), room, ui.theme().charset());
    ui.line(&format!("{INDENT}{ty}  {data}"))
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
}
