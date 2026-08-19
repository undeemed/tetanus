//! Every frame of the status line, at a walking pace.
//!
//! An offline turn finishes in milliseconds, so no still image of `tetanus run`
//! ever catches the spinner moving. This drives the same `Progress` renderer
//! slowly, so the live preview in `tools/uiwatch` can show what it does.
//!
//! Run it: `cargo run -p tetanus-ui --example status`.

use std::thread::sleep;
use std::time::Duration;

use tetanus_ui::{ColorChoice, Policy};

fn main() {
    let policy = Policy::from_process(ColorChoice::Auto);
    let mut progress = policy.stderr_progress();

    for phase in [
        "contacting mock-echo-1",
        "streaming the answer",
        "running echo",
    ] {
        progress.set(phase).ok();
        for _ in 0..6 {
            sleep(Duration::from_millis(80));
            progress.tick().ok();
        }
    }

    progress.finish().ok();
    policy
        .stdout()
        .note("the line erased itself; nothing was left behind")
        .ok();
}
