//! What a person sees when the binary stops being a command and becomes a
//! server.
//!
//! `tetanus serve` hands stdout to the protocol (contract §4.1), so every line
//! this module writes goes to stderr. That is not a detail of the
//! implementation: a frame the peer cannot parse is a broken session, and the
//! cheapest way to never write one is for the presentation layer to have no
//! access to the stream that carries them.
//!
//! # Why there is a banner at all
//!
//! A server that prints nothing and a server that failed to start look the
//! same from a terminal. The banner says which of the two happened, names the
//! directory the sessions will land in - the one setting that decides where
//! the work goes - and says how to stop it, because a process reading stdin
//! does not answer the key a user tries first.
//!
//! # Why it is written to a pipe as well
//!
//! It is content, not animation. The same rule the progress line follows: a
//! repainted frame is held back from a pipe, a sentence is not. An editor that
//! logs the binary's stderr gets the same three facts a person at a terminal
//! reads, which is what makes a bug report from either of them complete.

use std::io::{self, Write};
use std::path::Path;

use tetanus_ui::{Role, Ui};

/// What the server is about to do. Assembled by the caller: every field is a
/// resolved setting, and none of them is a rendering decision.
pub struct Serving<'a> {
    /// The carrier hosting the protocol.
    pub carrier: &'a str,
    /// Where the journals this server writes will land.
    pub sessions: &'a Path,
    /// The `major.minor` of the interface contract it speaks.
    pub protocol: &'a str,
}

/// Announce the server, before the first frame is read.
pub fn banner<W: Write>(ui: &mut Ui<W>, serving: &Serving) -> io::Result<()> {
    let carrier = ui.paint(Role::Accent, serving.carrier).to_string();
    ui.heading(&format!("tetanus serving on {carrier}"))?;

    let rows = [
        ("sessions", serving.sessions.display().to_string()),
        ("protocol", serving.protocol.to_string()),
    ];
    let label = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    for (name, said) in &rows {
        ui.field(name, label, said)?;
    }
    // Ctrl-C is what a user tries, and it is the wrong key: the process is
    // reading stdin, so the peer's end of file is what ends it cleanly.
    ui.note("end with Ctrl-D")?;
    ui.flush()
}

/// Say why the server stopped, so an exit is never mistaken for a crash.
///
/// End of file on stdin is the carrier's ordinary end, and it arrives with no
/// message of its own. Without this line a clean shutdown and a process that
/// died read identically at a terminal.
pub fn stopped<W: Write>(ui: &mut Ui<W>) -> io::Result<()> {
    ui.note("the peer closed stdin, so the server stopped")?;
    ui.flush()
}

/// Test Design Specification: what the server says for itself.
///
/// Features tested: that the banner names the carrier, the sessions directory
/// and the protocol version; that it says how to stop the process; and that
/// the closing line says why the process ended.
///
/// Features NOT tested here: that these lines land on stderr and never on
/// stdout (owned by `main.rs`, asserted end to end in `tests/serve.rs`), the
/// carrier itself (owned by `tetanus-rpc`), and the colour policy (owned by
/// `tetanus-ui`).
///
/// Environmental needs: none.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn rendered(serving: &Serving) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), 80);
        banner(&mut ui, serving).expect("render");
        ui.contents()
    }

    /// TC-CLI-SRV-1: the whole banner.
    /// Expected: the carrier in the title, the two settings in a straight
    /// column, and the key that ends the process. A user who cannot tell a
    /// started server from a failed one has to read the source to find out.
    #[test]
    fn the_banner_says_what_the_server_is_doing() {
        let told = rendered(&Serving {
            carrier: "stdio",
            sessions: Path::new("sessions"),
            protocol: "1.0",
        });

        assert_eq!(
            told,
            "\ntetanus serving on stdio\n\
             sessions  sessions\n\
             protocol  1.0\n\
             note: end with Ctrl-D\n"
        );
    }

    /// TC-CLI-SRV-2: a sessions directory that is not the default.
    /// Expected: the resolved path, as given. The banner is the only place a
    /// user reads where the work will land before any of it lands there.
    #[test]
    fn the_banner_names_the_directory_the_work_lands_in() {
        let told = rendered(&Serving {
            carrier: "stdio",
            sessions: Path::new("/srv/journals"),
            protocol: "1.0",
        });

        assert!(told.contains("sessions  /srv/journals"), "{told}");
    }

    /// TC-CLI-SRV-3: the closing line.
    /// Expected: a note naming end of file as the reason. A server that exits
    /// in silence reads as a crash, and the user's next move - re-run it, or
    /// report it - depends on telling the two apart.
    #[test]
    fn the_closing_line_says_why_the_server_ended() {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), 80);
        stopped(&mut ui).expect("render");

        assert_eq!(
            ui.contents(),
            "note: the peer closed stdin, so the server stopped\n"
        );
    }
}
