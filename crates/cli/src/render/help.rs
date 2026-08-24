//! Help ergonomics: what `tetanus --help` says, and how the colour policy
//! reaches clap before clap has parsed anything. Three things happen here
//! that are easy to get wrong.
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
//!
//! The page also has to say what the binary exits with. The numbers belong to
//! the interface contract, not to this module: `ErrorCode::exit_status` is
//! the single source, and [`EXIT_STATUS`] only words each one. It is on
//! `--help` alone ([`root_long_epilogue`]) because a status is read by the
//! script around a person, and `-h` is the summary that person skims for a
//! flag.

use clap::builder::styling::Styles;
use tetanus_ui::{wrap, ColorChoice, Role, Theme};

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

/// What to type, and what each one is for. The two columns are composed
/// rather than written out, so the gap between them is measured from the
/// widest command instead of counted by hand ([`examples`]).
const ROOT_EXAMPLES: &[(&str, &str)] = &[
    ("tetanus run", "one offline turn, mock adapter"),
    (
        "tetanus run \"list the files\"",
        "ask for something specific",
    ),
    (
        "tetanus run -a deepseek -m deepseek-v4-pro",
        "needs DEEPSEEK_API_KEY",
    ),
    ("tetanus chat", "talk to the model, turn by turn"),
    (
        "tetanus chat -a mock -s /tmp/c.jsonl",
        "the same offline, in that journal",
    ),
    ("tetanus sessions", "every journal, newest first"),
    ("tetanus sessions --ui", "pick one of them and read it"),
    (
        "tetanus replay sessions/turn.jsonl",
        "re-read a journal from before",
    ),
    (
        "tetanus replay sessions/turn.jsonl --live",
        "watch that turn arrive again",
    ),
    (
        "tetanus replay sessions/turn.jsonl --ui",
        "read it on a screen of its own",
    ),
    ("tetanus config", "every key, and what set it"),
    ("tetanus models", "which providers are reachable"),
    ("tetanus tools", "what the agent is able to call"),
    ("tetanus serve", "hand stdout to the protocol"),
    (
        "tetanus serve --listen 127.0.0.1:8787",
        "serve the protocol on a socket",
    ),
];

/// The same, for the one subcommand with enough flags to need its own.
const RUN_EXAMPLES: &[(&str, &str)] = &[
    ("tetanus run", "the default: \"run one full turn\""),
    ("tetanus run \"list the files\"", "ask for something else"),
    ("tetanus run - < task.md", "a prompt too long to quote"),
    ("tetanus run --trace", "the raw event sequence instead"),
    ("tetanus run --ui", "watch it on a screen of its own"),
    (
        "tetanus run --session /tmp/t.jsonl",
        "choose where the journal lands",
    ),
    ("tetanus run --max-steps 1", "stop after one step"),
    ("tetanus run --think", "unfold what the model thought"),
    ("tetanus run --json", "JSONL for a script, not for a person"),
];

/// And for the other one, which is a conversation rather than a command.
const CHAT_EXAMPLES: &[(&str, &str)] = &[
    ("tetanus chat", "DeepSeek; needs DEEPSEEK_API_KEY"),
    ("tetanus chat -a mock", "the same conversation, offline"),
    (
        "tetanus chat -s sessions/plan.jsonl",
        "start or resume that conversation",
    ),
    ("tetanus chat --think", "unfold what the model thought"),
    (
        "tetanus chat --max-steps 1",
        "stop each turn after one step",
    ),
];

/// An examples block, in two columns where they fit and stacked where they do
/// not.
///
/// Two columns are what makes the block scannable: the eye runs down the
/// commands, or down what each is for, and never reads a line to find out
/// which it is looking at. That needs the description column to start past the
/// widest command, which a narrow window has no room for.
///
/// Folded by clap instead, a row too wide is continued at column zero, where
/// the rest of a description is read as the start of another command. So a
/// window with no room for the second column gets the description under its
/// command, indented, and folded to what is left: two lines that say which is
/// which, rather than one wrapped into nonsense.
fn examples(theme: &Theme, width: usize, rows: &[(&str, &str)]) -> String {
    columns(theme, width, "Examples:", rows)
}

/// A headed block of two columns, stacked where the window has no room for
/// the second one.
///
/// The examples and the environment are the same shape - a thing to type, and
/// what it does - and they were not composed the same way. The environment
/// block was written out with its own spaces, so a narrow window handed it to
/// clap, which folds a row it is given at column zero: `--adapter deepseek`
/// arrived under `DEEPSEEK_API_KEY` in the column where a variable's name
/// goes, and reads as another variable.
fn columns(theme: &Theme, width: usize, heading: &str, rows: &[(&str, &str)]) -> String {
    let heading = theme.paint(Role::Heading, heading);
    // Two spaces in, and two clear of the widest command, which is where a
    // description starts when there is room for one beside it.
    let column = 2 + rows.iter().map(|(cmd, _)| cmd.len()).max().unwrap_or(0) + 2;
    let widest = rows.iter().map(|(_, what)| what.len()).max().unwrap_or(0);

    let mut lines = vec![heading.to_string()];
    if column + widest <= width {
        for (command, what) in rows {
            lines.push(format!("  {command:<0$}{what}", column - 2));
        }
    } else {
        for (command, what) in rows {
            // The command folds too, and under itself. A command wider than
            // the window is handed to clap whole otherwise, and clap folds it
            // at column zero, where the rest of `tetanus run -a deepseek -m
            // deepseek-v4-pro` starts a line that reads as another example.
            let mut folded = wrap(command, width.saturating_sub(2).max(1)).into_iter();
            let first = folded.next().unwrap_or_default();
            lines.push(format!("  {first}"));
            lines.extend(folded.map(|line| format!("    {line}")));
            lines.extend(
                wrap(what, width.saturating_sub(STACKED).max(1))
                    .into_iter()
                    .map(|line| format!("{}{line}", " ".repeat(STACKED))),
            );
        }
    }
    lines.join("\n")
}

/// How far a description is indented when it is under its command rather than
/// beside it. Deeper than the command, so the two are told apart at a glance,
/// and shallower than any column a wide window would have used.
const STACKED: usize = 6;

/// The block under the root help: what to type, and what the environment
/// changes. Upstream `dsh` closes its help with examples too, and it is the
/// fastest part of a help page to read.
///
/// Every variable this binary reads is listed, and the list is asserted
/// against the resolver that reads them: a user whose output came out plain,
/// or in ASCII, or the wrong width, comes to this page to find out which
/// variable did it, and one that is missing sends them to the source.
pub fn root_epilogue(theme: &Theme, width: usize) -> String {
    format!(
        "{}\n\n{}",
        examples(theme, width, ROOT_EXAMPLES),
        columns(theme, width, "Environment:", ENVIRONMENT),
    )
}

/// Every variable this binary reads, and what it changes.
///
/// The list is the resolver's, one row per variable `Env::from_process` and
/// the credential lookup ask for. A user whose output came out plain, or in
/// ASCII, or the wrong width, reads this page to find out which variable did
/// it, and one that is missing sends them to the source instead.
const ENVIRONMENT: &[(&str, &str)] = &[
    (
        "TETANUS_HOME",
        "where the settings document lives, unless `--settings` says",
    ),
    ("DEEPSEEK_API_KEY", "credential for `--adapter deepseek`"),
    ("NO_COLOR", "set to anything non-empty for plain output"),
    ("CLICOLOR_FORCE", "set to keep colour through a pipe"),
    ("CLICOLOR", "set to 0 for plain output"),
    ("TERM", "`dumb`, or unset: no colour, and ASCII glyphs"),
    (
        "LC_ALL, LC_CTYPE, LANG",
        "the first one set picks the glyphs; non-UTF-8 is ASCII",
    ),
    ("COLUMNS", "override the detected line width"),
];

/// The block under `tetanus chat --help`, and what a conversation is.
pub fn chat_epilogue(theme: &Theme, width: usize) -> String {
    format!(
        "{}\n\n{}",
        examples(theme, width, CHAT_EXAMPLES),
        wrap(
            "Type a message and press Enter. `/help` lists the commands, \
             `/exit` or ctrl-d leaves, and every turn is appended to the \
             journal, which is what the next chat on the same path reads back \
             as memory.",
            width,
        )
        .join("\n"),
    )
}

/// What each exit status means, in the order a reader scans them.
///
/// Interface contract section 4.5 fixes the numbers and `ErrorCode::exit_status`
/// is the single source for them, so nothing here decides one. Several codes
/// share a status - a session that is gone, a session that is busy and a tool
/// this build has not got are all `4` - so a row says what they have in
/// common rather than naming codes a reader of a help page has never seen.
/// `0` is worded here too, though no failure carries it, because the caller
/// checking the others has to know what the absence of one means.
///
/// A status the contract defines and this table does not word is a hole in
/// the page, and TC-CLI-HELP-8 is what stops one being left.
const EXIT_STATUS: &[(u8, &str)] = &[
    (0, "it did what was asked"),
    (1, "this build failed, or a file could not be read"),
    (2, "the command line was wrong"),
    (3, "this build cannot do what was asked of it"),
    (4, "what was named is not there, or is busy"),
    (5, "a credential is not set"),
    (6, "the provider refused the call"),
    (130, "interrupted"),
];

/// The width of the number column: two spaces of indent, room for `130`, and
/// the space after it. What a meaning starts at, and what a folded one
/// continues under.
const NUMBER: usize = 7;

/// The root epilogue, and under it the statuses a caller reads.
///
/// Only `--help` is given this one. The rows are aligned on a fixed column
/// rather than on the widest number, because the widest is `130` and a page
/// that moves its own column when a status is added reads as a different page.
///
/// The meanings are folded here rather than left to clap. clap folds an
/// epilogue it is handed to the width it was given, but it folds every line
/// back to column zero, and a row continued in the number column reads as a
/// status whose number went missing. Folded to `width` with the number column
/// kept clear, nothing is left for clap to fold.
pub fn root_long_epilogue(theme: &Theme, width: usize) -> String {
    let heading = theme.paint(Role::Heading, "Exit status:");
    let indent = " ".repeat(NUMBER);
    let mut rows = Vec::new();
    for (status, meaning) in EXIT_STATUS {
        let mut folded = wrap(meaning, width.saturating_sub(NUMBER).max(1)).into_iter();
        let first = folded.next().unwrap_or_default();
        rows.push(format!("  {status:<4} {first}"));
        rows.extend(folded.map(|line| format!("{indent}{line}")));
    }
    format!(
        "{}\n\n{heading}\n{}",
        root_epilogue(theme, width),
        rows.join("\n")
    )
}

/// The block under `tetanus run --help`.
pub fn run_epilogue(theme: &Theme, width: usize) -> String {
    examples(theme, width, RUN_EXAMPLES)
}
