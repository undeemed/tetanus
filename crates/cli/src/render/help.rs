//! How the colour policy reaches clap before clap has parsed anything. Two
//! things happen here that are easy to get wrong.
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
use tetanus_ui::{ColorChoice, Role};

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
