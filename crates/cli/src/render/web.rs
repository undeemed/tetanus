//! What a person reads when the browser panel comes up.
//!
//! The same three jobs `render::serve`'s banner does - say that it started,
//! say where the work lands, say how to stop it - plus the one fact this
//! composition has and that one does not: the address to open. It is the whole
//! reason a person ran this subcommand, so it is the first line and it is
//! written whole, not folded into a sentence.
//!
//! Everything goes to stderr, like `serve`'s banner, so that a shell wrapping
//! this in a pipeline reads the same three facts and the page's own bytes are
//! never mixed with them.

use std::io::{self, Write};
use std::path::Path;

use tetanus_ui::{tame_line, Role, Ui};

/// What is being served, and from where.
pub struct Serving<'a> {
    pub page: &'a str,
    pub carrier: &'a str,
    pub sessions: &'a Path,
    pub frontend: &'a str,
}

/// Say what came up.
pub fn banner<W: Write>(ui: &mut Ui<W>, serving: &Serving<'_>) -> io::Result<()> {
    ui.heading("web")?;
    let width = "frontend".len();
    ui.field("open", width, &tame_line(serving.page))?;
    ui.field("carrier", width, &tame_line(serving.carrier))?;
    ui.field(
        "sessions",
        width,
        &tame_line(&serving.sessions.display().to_string()),
    )?;
    ui.field("frontend", width, &tame_line(serving.frontend))?;
    let said = ui.paint(Role::Muted, "ctrl-c to stop").to_string();
    ui.line(&said)
}

/// Say it stopped, for the same reason the banner says it started: a server
/// that prints nothing on the way out and one that was killed look the same.
pub fn stopped<W: Write>(ui: &mut Ui<W>) -> io::Result<()> {
    let said = ui.paint(Role::Muted, "the web panel stopped").to_string();
    ui.line(&said)
}
