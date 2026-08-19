//! A journal played back at a pace a person can watch.
//!
//! `tetanus replay` prints a finished turn all at once, which is what you want
//! when you are reading it. `--live` answers the other question - what did
//! this turn look like while it was happening - by feeding the same journal
//! through the same view `tetanus run` draws, one event at a time.
//!
//! Nothing here is a second renderer. [`Live`] settles the lines and composes
//! the block exactly as it does for a running turn; this module only decides
//! when the next event is allowed to arrive.
//!
//! # Pace
//!
//! The recorded gaps alone will not do. An offline turn writes every one of
//! its events inside the same millisecond, so playing the recording faithfully
//! would flash the whole turn past inside one frame, and a turn that waited
//! four minutes on a slow model would stall the playback for four minutes. A
//! gap is therefore clamped into [`FLOOR`]..[`CEILING`] and only then divided
//! by `speed`: the recording sets the rhythm, the clamp keeps it watchable,
//! and `--speed 0.5` still slows down a turn that is already at the floor.
//!
//! # Clock
//!
//! The footer counts the journal's own time, not the playback's. A replay that
//! reported how long it took to play would say nothing about the turn it is
//! showing, so an offline turn honestly reads `0.0s` however slowly it plays.
//!
//! # Into a pipe
//!
//! Nothing waits when the stream is not a terminal, and `Screen` draws nothing
//! there, so `replay --live` into a pipe writes what `replay` writes, at once.
//! There is no one watching to pace for, and the byte-identical guarantee the
//! rest of the surface keeps holds here too.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use tetanus_protocol::types::SessionEvent;
use tetanus_ui::{Screen, Ui};

use super::live::Live;

/// How often the block is redrawn while nothing is arriving. Matches the poll
/// interval of a live run, so the spinner turns at one speed everywhere.
const FRAME: Duration = Duration::from_millis(80);

/// The shortest a gap may become. Every event of an offline turn carries the
/// same millisecond, and a whole turn inside one frame is not a playback.
const FLOOR: Duration = Duration::from_millis(140);

/// The longest a gap may become. A recorded wait is worth showing; sitting
/// through it again is not.
const CEILING: Duration = Duration::from_secs(2);

/// Play `events` through the live view.
///
/// `animated` is "this stream is a terminal", resolved once by the caller -
/// it decides both whether the block is drawn and whether anything waits.
pub fn play<W: Write>(
    ui: &mut Ui<W>,
    animated: bool,
    events: &[SessionEvent],
    speed: f64,
) -> io::Result<()> {
    let (theme, width) = (*ui.theme(), ui.width());
    let mut view = Live::new(theme, width, "replaying");
    let mut screen = Screen::new(Ui::new(ui.out(), theme, width), animated);

    let start = events.first().map(|event| event.time).unwrap_or_default();
    let mut previous: Option<u64> = None;
    for event in events {
        if let Some(before) = previous.filter(|_| animated) {
            let gap = pace(event.time.saturating_sub(before), speed);
            hold(&mut screen, &mut view, since(start, before), gap)?;
        }
        previous = Some(event.time);
        let lines = view.push(event);
        screen.print(&lines)?;
    }
    screen.finish()?;
    Ok(())
}

/// Hold the block on screen for `gap`, ticking it while it waits.
///
/// The frame is drawn before the sleep, not after, so the block an event
/// produced is on screen for the whole of the gap that follows it.
fn hold<W: Write>(
    screen: &mut Screen<W>,
    view: &mut Live,
    elapsed: Duration,
    gap: Duration,
) -> io::Result<()> {
    let until = Instant::now() + gap;
    loop {
        view.tick();
        screen.draw(&view.block(elapsed))?;
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(FRAME));
    }
}

/// What one recorded gap becomes on the playback clock.
fn pace(gap: u64, speed: f64) -> Duration {
    Duration::from_millis(gap)
        .clamp(FLOOR, CEILING)
        .div_f64(speed)
}

/// How far into the turn a recorded moment is.
fn since(start: u64, time: u64) -> Duration {
    Duration::from_millis(time.saturating_sub(start))
}

/// Test Design Specification: playing a journal back.
///
/// Features tested: the pace one recorded gap becomes, at the floor, at the
/// ceiling and under `--speed`; that a playback into a pipe is the timeline
/// byte for byte; and that an empty journal plays as nothing.
///
/// Features NOT tested here: the wording of a settled line (owned by
/// `timeline.rs`), the block (owned by `live.rs`), the escapes a frame writes
/// (owned by `tetanus-ui`'s `screen.rs`), and the wall-clock duration of an
/// animated playback - a case that sleeps to prove it slept buys a slow,
/// flaky suite and no new information, because the arithmetic under it is
/// TC-CLI-PLAY-1..3.
///
/// Environmental needs: none. No case here waits, because no case here is
/// animated.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    fn event(seq: u64, time: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            ty: ty.into(),
            seq,
            time,
            data,
            source_event_seqs: None,
        }
    }

    fn turn() -> Vec<SessionEvent> {
        vec![
            event(0, 1_000, "turn/start", json!({ "turn": 1 })),
            event(1, 1_000, "step/start", json!({ "turn": 1, "step": 1 })),
            event(2, 1_000, "user/message", json!({ "content": "echo this" })),
            event(
                3,
                1_020,
                "assistant/chunk",
                json!({ "chunk": "text", "delta": "on it", "turn": 1, "step": 1 }),
            ),
            event(4, 1_400, "assistant/message", json!({ "content": "on it" })),
            event(
                5,
                9_999,
                "turn/end",
                json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
            ),
        ]
    }

    /// TC-CLI-PLAY-1: a gap shorter than the floor.
    /// Expected: the floor. Two events written in the same millisecond - every
    /// pair in an offline journal - are still shown one after the other.
    #[test]
    fn a_gap_is_never_shorter_than_the_floor() {
        assert_eq!(pace(0, 1.0), FLOOR);
        assert_eq!(pace(139, 1.0), FLOOR);
        assert_eq!(pace(200, 1.0), Duration::from_millis(200));
    }

    /// TC-CLI-PLAY-2: a gap longer than the ceiling.
    /// Expected: the ceiling. A recorded four-minute wait is reported by the
    /// clock in the footer, not by making the viewer sit through it again.
    #[test]
    fn a_gap_is_never_longer_than_the_ceiling() {
        assert_eq!(pace(240_000, 1.0), CEILING);
        assert_eq!(pace(2_001, 1.0), CEILING);
    }

    /// TC-CLI-PLAY-3: `--speed` against the clamp.
    /// Expected: the clamp settles the gap first and the speed divides what it
    /// settled, so `0.5` slows a floor-length gap down instead of leaving it
    /// pinned at the floor.
    #[test]
    fn speed_divides_what_the_clamp_settled() {
        assert_eq!(pace(0, 2.0), FLOOR / 2);
        assert_eq!(pace(0, 0.5), FLOOR * 2);
        assert_eq!(pace(1_000, 4.0), Duration::from_millis(250));
        assert_eq!(pace(240_000, 4.0), CEILING / 4);
    }

    /// TC-CLI-PLAY-4: a playback into a pipe.
    /// Expected: the bytes `tetanus replay` writes for the same journal, and
    /// no escape codes. `--live` is a way of watching a turn, not a second
    /// wording of it, so a script that reads either gets one answer.
    #[test]
    fn a_piped_playback_is_the_timeline() {
        let events = turn();
        let mut played = buffered(theme(), 80);
        play(&mut played, false, &events, 1.0).expect("play");

        let mut printed = buffered(theme(), 80);
        super::super::timeline::render(&mut printed, &events).expect("render");

        assert_eq!(played.contents(), printed.contents());
        assert!(!played.contents().contains('\u{1b}'), "escapes in a pipe");
    }

    /// TC-CLI-PLAY-5: a journal with no events.
    /// Expected: nothing written and no panic. The first event's time is what
    /// the clock counts from, and a journal that has none must not decide the
    /// arithmetic by crashing.
    #[test]
    fn an_empty_journal_plays_as_nothing() {
        let mut ui = buffered(theme(), 80);
        play(&mut ui, false, &[], 1.0).expect("play");
        assert_eq!(ui.contents(), "");
    }
}
