//! What this build is, for the person about to report that it is broken.
//!
//! `tetanus info` makes no calls (contract §4.7): everything on the page is
//! known before the process talks to anything. The page answers the three
//! questions a bug report needs answered before anyone can act on it - which
//! build, which protocol, and what it was able to reach - and nothing else.
//!
//! # Why the counts, and not the lists
//!
//! A provider list and a tool list already have pages of their own, and
//! repeating them here would give a reader two places to look and one of them
//! would eventually be wrong. A count plus the command that expands it says
//! the same thing without the second copy: "two providers" is what a bug
//! report needs, and `tetanus models` is one keystroke away.
//!
//! # Why the protocol version is on it
//!
//! It is the one number that decides whether this binary and a server can
//! speak at all - a server refuses a client whose major differs. When that
//! refusal happens, this page is where the user reads what they have.

use std::io::{self, Write};

use tetanus_ui::{Role, Ui};

/// Space between the label column and its value.
const GAP: usize = 2;

/// The facts on the page. Assembled by the caller, because every one of them
/// is something the binary knows about itself and none of them is a rendering
/// decision.
pub struct Build {
    /// This binary's version.
    pub version: &'static str,
    /// The `major.minor` of the interface contract it was built against.
    pub protocol: &'static str,
    /// How many model providers it registers.
    pub providers: usize,
    /// How many tools an agent may call.
    pub tools: usize,
    pub os: &'static str,
    pub arch: &'static str,
}

/// Render the build page.
pub fn render<W: Write>(ui: &mut Ui<W>, build: &Build) -> io::Result<()> {
    let title = ui.paint(Role::Accent, build.version).to_string();
    ui.heading(&format!("tetanus {title}"))?;

    let rows = [
        ("protocol", build.protocol.to_string(), None),
        (
            "providers",
            build.providers.to_string(),
            Some("tetanus models"),
        ),
        ("tools", build.tools.to_string(), Some("tetanus tools")),
        ("platform", format!("{} {}", build.os, build.arch), None),
    ];
    let label = rows
        .iter()
        .map(|(label, ..)| label.chars().count())
        .max()
        .unwrap_or(0);
    // Only the rows that carry a command share a value column. Measuring the
    // rows that do not would push the commands out past the widest fact on
    // the page, and the two of them would stop reading as a pair.
    let value = rows
        .iter()
        .filter(|(_, _, expand)| expand.is_some())
        .map(|(_, value, _)| value.chars().count())
        .max()
        .unwrap_or(0);

    for (name, said, expand) in &rows {
        // The command that expands a count is painted after the padding, so a
        // coloured page and a plain one break in the same column.
        let text = match expand {
            Some(command) => {
                let pad = " ".repeat(value.saturating_sub(said.chars().count()) + GAP);
                let hint = ui.paint(Role::Muted, command).to_string();
                format!("{said}{pad}{hint}")
            }
            None => said.clone(),
        };
        ui.field(name, label, &text)?;
    }
    Ok(())
}

/// Test Design Specification: the build page.
///
/// Features tested: that every fact the page promises is on it, that the two
/// counts carry the command that expands them, and that the columns line up.
///
/// Features NOT tested here: whether the counts are the real ones (owned by
/// `main.rs`, and asserted against the catalogues end to end in
/// `tests/presentation.rs`) and the colour policy (owned by `tetanus-ui`).
///
/// Environmental needs: none.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn build() -> Build {
        Build {
            version: "0.1.0",
            protocol: "1.0",
            providers: 2,
            tools: 1,
            os: "linux",
            arch: "x86_64",
        }
    }

    fn rendered(build: &Build) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), 80);
        render(&mut ui, build).expect("render");
        ui.contents()
    }

    /// TC-CLI-INFO-1: the whole page.
    /// Expected: the version in the title, then one row per fact with the
    /// labels and values in two straight columns, and the expanding command
    /// beside each count. A bug report pasted from this page has to carry the
    /// build and the protocol without the reporter being asked twice.
    #[test]
    fn the_page_carries_what_a_bug_report_needs() {
        assert_eq!(
            rendered(&build()),
            "\ntetanus 0.1.0\n\
             protocol   1.0\n\
             providers  2  tetanus models\n\
             tools      1  tetanus tools\n\
             platform   linux x86_64\n"
        );
    }

    /// TC-CLI-INFO-2: a build that registers nothing.
    /// Expected: `0`, and the command still beside it. A count of zero is the
    /// answer to "why did my run find no tools", so it is the one count that
    /// must never be hidden.
    #[test]
    fn a_build_that_registers_nothing_says_zero() {
        let empty = Build {
            providers: 0,
            tools: 0,
            ..build()
        };
        let page = rendered(&empty);

        assert!(page.contains("providers  0"), "{page}");
        assert!(page.contains("tools      0"), "{page}");
        assert!(page.contains("tetanus tools"), "{page}");
    }
}
