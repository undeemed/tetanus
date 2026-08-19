//! A live block, at a walking pace.
//!
//! `Screen` repaints in place, so a still image of a finished run shows only
//! the last frame, and a piped run shows none of them at all. This drives the
//! renderer slowly, so the live preview in `tools/uiwatch` can show the frames
//! it writes: an answer arriving a word at a time, a tool row appearing under
//! it, and the block shrinking again when the tool is done.
//!
//! Run it: `cargo run -p tetanus-ui --example screen`.

use std::thread::sleep;
use std::time::Duration;

use tetanus_ui::{ColorChoice, Policy, Role, Theme};

const ANSWER: &str = "Let me echo that back for you.";

fn main() {
    let policy = Policy::from_process(ColorChoice::Auto);
    let theme = policy.stdout;
    let mut screen = policy.stdout_screen();

    let mut shown = String::new();
    for word in ANSWER.split_inclusive(' ') {
        shown.push_str(word);
        screen
            .draw(&[said(&theme, &shown), footer(&theme, "streaming")])
            .ok();
        sleep(Duration::from_millis(140));
    }

    // A tool row joins the block, so the block is three rows for a while.
    for frame in 0..6 {
        let glyph = theme.glyph("▸", ">");
        screen
            .draw(&[
                said(&theme, ANSWER),
                format!("  {} {}", theme.paint(Role::Tool, glyph), "echo"),
                footer(&theme, "running echo"),
            ])
            .ok();
        sleep(Duration::from_millis(140));
        let _ = frame;
    }

    // The tool is done and its row leaves, which is the frame that has to
    // erase a row rather than overwrite one.
    screen
        .draw(&[said(&theme, ANSWER), footer(&theme, "closing the turn")])
        .ok();
    sleep(Duration::from_millis(400));

    // Only what is committed survives the block, and only committed lines
    // reach a pipe.
    screen
        .print(&[
            said(&theme, ANSWER),
            format!(
                "  {} echo  {}",
                theme.paint(Role::Ok, theme.glyph("✓", "+")),
                "run one full turn"
            ),
        ])
        .ok();
    let mut ui = screen.finish().expect("hand the stream back");
    ui.line(&format!(
        "\n{}",
        theme.paint(Role::Muted, "turn 1 · natural · 1 step")
    ))
    .ok();
}

fn said(theme: &Theme, text: &str) -> String {
    format!("  {}    {text}", theme.paint(Role::Topic, "ai"))
}

fn footer(theme: &Theme, phase: &str) -> String {
    format!("  {}", theme.paint(Role::Muted, phase))
}
