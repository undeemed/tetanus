//! What a person sees when the binary stops being a command and becomes a
//! server.
//!
//! `tetanus serve` hands stdout to the protocol (contract §4.1), so every line
//! this module writes goes to stderr. That is not a detail of the
//! implementation: a frame the peer cannot parse is a broken session, and the
//! cheapest way to never write one is for the presentation layer to have no
//! access to the stream that carries them.
//!
//! The WebSocket carrier does not use stdout at all, so its page could go
//! there. It does not. `tetanus serve` is one subcommand and a user who
//! learned where to read it on one carrier has learned it on both, which is
//! worth more than a stream nobody was going to use.
//!
//! # Why there is a banner at all
//!
//! A server that prints nothing and a server that failed to start look the
//! same from a terminal. The banner says which of the two happened, names the
//! directory the sessions will land in - the one setting that decides where
//! the work goes - and says how to stop it, which is not the same key on both
//! carriers.
//!
//! On the WebSocket carrier it also names the address, and that is the whole
//! reason the banner is printed after the socket is bound rather than before.
//! `--listen 127.0.0.1:0` asks the operating system to choose a port, so the
//! address the user asked for is not the address anyone can connect to. The
//! banner prints the one that was bound.
//!
//! # Why it is written to a pipe as well
//!
//! It is content, not animation. The same rule the progress line follows: a
//! repainted frame is held back from a pipe, a sentence is not. An editor that
//! logs the binary's stderr gets the same three facts a person at a terminal
//! reads, which is what makes a bug report from either of them complete.

use std::io::{self, Write};
use std::path::Path;

use tetanus_ui::{tame_line, Role, Ui};

/// Which carrier is hosting the protocol, and what a person needs to know
/// about it.
///
/// A carrier is not a label here. It decides two things a user acts on: where
/// to point a peer, and which key ends the process. Carrying them together is
/// what stops a banner from telling someone to press Ctrl-D at a server that
/// is not reading stdin.
#[derive(Clone, Copy)]
pub enum Carrier<'a> {
    /// stdin and stdout, one frame per line.
    Stdio,
    /// A WebSocket server, on the address it actually bound.
    WebSocket(&'a str),
}

impl Carrier<'_> {
    /// The carrier's name, as the banner says it.
    fn name(&self) -> &'static str {
        match self {
            Carrier::Stdio => "stdio",
            Carrier::WebSocket(_) => "websocket",
        }
    }

    /// The key that ends this server.
    fn stop_key(&self) -> &'static str {
        match self {
            // The process is reading stdin, so the peer's end of file is what
            // ends it cleanly - not the key a user tries first.
            Carrier::Stdio => "Ctrl-D",
            // Nothing is reading stdin, so end of file means nothing and the
            // key a user tries first is the right one.
            Carrier::WebSocket(_) => "Ctrl-C",
        }
    }
}

/// What the server is about to do. Assembled by the caller: every field is a
/// resolved setting, and none of them is a rendering decision.
pub struct Serving<'a> {
    /// The carrier hosting the protocol.
    pub carrier: Carrier<'a>,
    /// Where the journals this server writes will land.
    pub sessions: &'a Path,
    /// The `major.minor` of the interface contract it speaks.
    pub protocol: &'a str,
}

/// Announce the server, before the first frame is read.
pub fn banner<W: Write>(ui: &mut Ui<W>, serving: &Serving) -> io::Result<()> {
    let carrier = ui.paint(Role::Accent, serving.carrier.name()).to_string();
    ui.heading(&format!("tetanus serving on {carrier}"))?;

    let mut rows = Vec::new();
    // First, because on this carrier it is the one fact a peer cannot work
    // out for itself: with `--listen 127.0.0.1:0` the port was chosen by the
    // operating system a moment ago.
    if let Carrier::WebSocket(address) = serving.carrier {
        rows.push(("address", address.to_string()));
    }
    // The directory came off `--dir`, so it is tamed like any other value
    // drawn as one whole row. The address above it did not: it is what the
    // socket reported after it bound, which is this build's own.
    rows.push((
        "sessions",
        tame_line(&serving.sessions.display().to_string()),
    ));
    rows.push(("protocol", serving.protocol.to_string()));
    let label = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    for (name, said) in &rows {
        ui.field(name, label, said)?;
    }
    ui.note(&format!("end with {}", serving.carrier.stop_key()))?;
    ui.flush()
}

/// Say why the server stopped, so an exit is never mistaken for a crash.
///
/// Each carrier has an ordinary end, and neither arrives with a message of its
/// own. Without this line a clean shutdown and a process that died read
/// identically at a terminal.
pub fn stopped<W: Write>(ui: &mut Ui<W>, carrier: Carrier) -> io::Result<()> {
    let why = match carrier {
        Carrier::Stdio => "the peer closed stdin",
        // Not "the peer hung up": a WebSocket server outlives any one peer,
        // and the thing that ended it was the interrupt.
        Carrier::WebSocket(_) => "interrupted",
    };
    ui.note(&format!("{why}, so the server stopped"))?;
    ui.flush()
}

/// Test Design Specification: what the server says for itself.
///
/// Features tested: that the banner names the carrier, the sessions directory
/// and the protocol version; that the WebSocket carrier also names the address
/// it bound; that the banner says how to stop the process, which is a
/// different key on each carrier; that the closing line says why the process
/// ended, which is a different reason on each; and that a directory named with
/// escape sequences is still drawn as one row.
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
            carrier: Carrier::Stdio,
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
            carrier: Carrier::Stdio,
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
        stopped(&mut ui, Carrier::Stdio).expect("render");

        assert_eq!(
            ui.contents(),
            "note: the peer closed stdin, so the server stopped\n"
        );
    }

    /// TC-CLI-SRV-4: the whole banner on the WebSocket carrier.
    /// Expected: the address first and the column widened to fit its label.
    /// The address leads because it is the one fact a peer cannot work out
    /// for itself - with `--listen 127.0.0.1:0` the port was chosen by the
    /// operating system a moment before this line was written.
    #[test]
    fn the_websocket_banner_leads_with_the_address_it_bound() {
        let told = rendered(&Serving {
            carrier: Carrier::WebSocket("127.0.0.1:34567"),
            sessions: Path::new("sessions"),
            protocol: "1.0",
        });

        assert_eq!(
            told,
            "\ntetanus serving on websocket\n\
             address   127.0.0.1:34567\n\
             sessions  sessions\n\
             protocol  1.0\n\
             note: end with Ctrl-C\n"
        );
    }

    /// TC-CLI-SRV-5: the closing line on the WebSocket carrier.
    /// Expected: "interrupted", not "the peer closed stdin". A WebSocket
    /// server outlives any one peer and is not reading stdin at all, so the
    /// stdio wording would name a thing that did not happen.
    #[test]
    fn the_websocket_closing_line_names_the_interrupt() {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), 80);
        stopped(&mut ui, Carrier::WebSocket("127.0.0.1:34567")).expect("render");

        assert_eq!(ui.contents(), "note: interrupted, so the server stopped\n");
    }

    /// TC-CLI-SRV-6: a sessions directory whose name carries escape sequences.
    /// Expected: the banner is four lines and the row holds the name with the
    /// sequences taken out. `--dir` takes whatever a shell can quote, and this
    /// banner is the first thing the server prints: a name that cleared the
    /// screen here would take the two rows under it with it, and a line feed
    /// in one would put `protocol` under a row that never said what it was.
    #[test]
    fn a_directory_named_with_an_escape_sequence_stays_one_row() {
        let told = rendered(&Serving {
            carrier: Carrier::Stdio,
            sessions: Path::new("na\u{1b}[2Jsty\u{1b}]0;pwned\u{7}\nlogs"),
            protocol: "1.0",
        });

        assert_eq!(
            told,
            "\ntetanus serving on stdio\n\
             sessions  nasty logs\n\
             protocol  1.0\n\
             note: end with Ctrl-D\n"
        );
    }
}
