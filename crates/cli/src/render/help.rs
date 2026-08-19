//! Help ergonomics: what `tetanus --help` says, and how the colour policy
//! reaches clap before clap has parsed anything. Two things happen here that
//! are easy to get wrong.
//!
//! Help is printed *during* parsing, so the palette has to be decided before
//! `Cli::parse()` runs. [`color_from_argv`] pre-scans the raw arguments for
//! `--color`, the way `cargo` does, and the result configures the command
//! before it is asked to render anything.
//!
//! And clap has a colour policy of its own. It is switched off here
//! ([`command_style`]) whenever we resolved plain, so that one decision - the
//! one in `tetanus-ui` - governs every byte the binary writes, help text
//! included.

use clap::builder::styling::Styles;
use tetanus_ui::{ColorChoice, Role, Theme};

/// Pre-scan raw arguments for `--color`, before clap owns them.
///
/// Accepts `--color WHEN` and `--color=WHEN`. An unknown or missing value
/// resolves to `Auto`: rejecting it is clap's job a moment later, with a
/// proper usage error, and this pass must not print anything of its own.
pub fn color_from_argv<I, T>(args: I) -> ColorChoice
where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let mut wants_value = false;
    for arg in args {
        let arg = arg.as_ref();
        let value = if wants_value {
            arg
        } else if let Some(value) = arg.strip_prefix("--color=") {
            value
        } else if arg == "--color" {
            wants_value = true;
            continue;
        } else {
            continue;
        };
        return match value {
            "always" => ColorChoice::Always,
            "never" => ColorChoice::Never,
            _ => ColorChoice::Auto,
        };
    }
    ColorChoice::Auto
}

/// The clap palette, taken from the same roles the rest of the surface uses,
/// so help text and run output speak one visual language.
pub fn styles() -> Styles {
    Styles::styled()
        .header(Role::Heading.style())
        .usage(Role::Heading.style())
        .literal(Role::Accent.style())
        .placeholder(Role::Muted.style())
        .valid(Role::Ok.style())
        .invalid(Role::Warn.style())
        .error(Role::Error.style())
}

/// Whether clap may emit its own escape codes. Our policy already decided;
/// clap must not second-guess it in either direction.
pub fn command_style(color: bool) -> clap::ColorChoice {
    if color {
        clap::ColorChoice::Always
    } else {
        clap::ColorChoice::Never
    }
}

/// The block under the root help: what to type, and what the environment
/// changes. Upstream `dsh` closes its help with examples too, and it is the
/// fastest part of a help page to read.
pub fn root_epilogue(theme: &Theme) -> String {
    let examples = theme.paint(Role::Heading, "Examples:");
    let environment = theme.paint(Role::Heading, "Environment:");
    format!(
        "\
{examples}
  tetanus run                                 one offline turn, mock adapter
  tetanus run \"list the files\"                ask for something specific
  tetanus run -a deepseek -m deepseek-v4-pro  needs DEEPSEEK_API_KEY
  tetanus sessions                            every journal, newest first
  tetanus sessions --ui                       the same list, with a cursor
  tetanus replay sessions/turn.jsonl          re-read a journal from before
  tetanus replay sessions/turn.jsonl --live   watch that turn arrive again
  tetanus replay sessions/turn.jsonl --ui     read it on a screen of its own
  tetanus config                              every key, and what set it
  tetanus models                              which providers are reachable
  tetanus tools                               what the agent is able to call
  tetanus serve                               hand stdout to the protocol
  tetanus serve --listen 127.0.0.1:8787       serve the protocol on a socket

{environment}
  DEEPSEEK_API_KEY  credential for `--adapter deepseek`
  NO_COLOR          set to anything non-empty for plain output
  CLICOLOR_FORCE    set to keep colour through a pipe
  COLUMNS           override the detected line width"
    )
}

/// The block under `tetanus run --help`.
pub fn run_epilogue(theme: &Theme) -> String {
    let examples = theme.paint(Role::Heading, "Examples:");
    format!(
        "\
{examples}
  tetanus run                         the default: \"run one full turn\"
  tetanus run \"list the files\"        ask for something else
  tetanus run - < task.md             a prompt too long to quote
  tetanus run --trace                 the raw event sequence instead
  tetanus run --ui                    watch it on a screen of its own
  tetanus run --session /tmp/t.jsonl  choose where the journal lands
  tetanus run --max-steps 1           stop after one step
  tetanus run --think                 unfold what the model thought
  tetanus run --json                  JSONL for a script, not for a person"
    )
}
