//! Color and charset policy: one decision, made once per output stream, from
//! the invocation's flag and the environment.
//!
//! The precedence below is the ecosystem's, not ours. It matches what
//! `anstream` resolves for the rest of the Rust CLI world, so `tetanus`
//! answers `NO_COLOR=1` and `CLICOLOR_FORCE=1` the same way `cargo` does:
//!
//! 1. an explicit `--color always|never` on this invocation,
//! 2. `NO_COLOR` set to a non-empty value - color off,
//! 3. `CLICOLOR_FORCE` set to anything but `0` - color on,
//! 4. `TERM=dumb` - color off,
//! 5. `CLICOLOR=0` - color off,
//! 6. otherwise: color on exactly when this stream is a terminal.
//!
//! Nothing here reads the process environment on its own. [`Env`] is a plain
//! value the caller fills, so a test states the world it means instead of
//! mutating the process it shares with every other test.

/// What the invocation asked for. `Auto` defers to [`Env`] and the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    /// The flag values the CLI accepts, in help order.
    pub const NAMES: [&'static str; 3] = ["auto", "always", "never"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// The environment variables that decide color and charset, as values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    pub no_color: Option<String>,
    pub clicolor: Option<String>,
    pub clicolor_force: Option<String>,
    pub term: Option<String>,
    /// The first of `LC_ALL`, `LC_CTYPE`, `LANG` that is set.
    pub locale: Option<String>,
    /// `COLUMNS`, honored before asking the terminal for its width.
    pub columns: Option<String>,
}

impl Env {
    /// Read the process environment. The only place in the crate that does.
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            no_color: var("NO_COLOR"),
            clicolor: var("CLICOLOR"),
            clicolor_force: var("CLICOLOR_FORCE"),
            term: var("TERM"),
            locale: var("LC_ALL")
                .or_else(|| var("LC_CTYPE"))
                .or_else(|| var("LANG")),
            columns: var("COLUMNS"),
        }
    }

    fn set(value: &Option<String>) -> bool {
        value.as_deref().is_some_and(|v| !v.is_empty())
    }

    fn is_dumb(&self) -> bool {
        self.term.as_deref() == Some("dumb")
    }
}

/// Resolve whether one stream gets ANSI styling.
///
/// # Arguments
/// * `choice` - what `--color` asked for.
/// * `env` - the environment as read once at startup.
/// * `is_terminal` - whether *this* stream is a terminal. stdout being a pipe
///   must not strip color from stderr, so the caller decides per stream.
pub fn color_enabled(choice: ColorChoice, env: &Env, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => return true,
        ColorChoice::Never => return false,
        ColorChoice::Auto => {}
    }
    if Env::set(&env.no_color) {
        return false;
    }
    if env.clicolor_force.as_deref().is_some_and(|v| v != "0") {
        return true;
    }
    if env.is_dumb() {
        return false;
    }
    if env.clicolor.as_deref() == Some("0") {
        return false;
    }
    is_terminal
}

/// Whether a terminal can hold a full-screen view.
///
/// A screen is not written with characters, it is written with cursor moves:
/// the alternate screen, absolute addressing, and an erase per row. `TERM=dumb`
/// is a terminal saying it answers none of that, and a terminal with no `TERM`
/// at all has not said it answers any - both are asking for the pages this
/// binary prints, not for a page it repaints.
///
/// Colour asks the same question about the same variable and is a separate
/// answer: a terminal that cannot address the cursor may still show colour,
/// and one that shows no colour may address it perfectly.
pub fn addressable(env: &Env) -> bool {
    !env.is_dumb() && Env::set(&env.term)
}

/// Which glyphs the renderer may draw. A dumb terminal or a non-UTF-8 locale
/// gets the ASCII set, so a spinner never lands as mojibake in a log file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    Unicode,
    Ascii,
}

/// Resolve the drawing charset from the same environment snapshot.
pub fn charset(env: &Env) -> Charset {
    if env.is_dumb() {
        return Charset::Ascii;
    }
    match env.locale.as_deref() {
        Some(locale) if locale.to_ascii_lowercase().contains("utf") => Charset::Unicode,
        // No locale at all is the common container case; assume the modern default.
        None => Charset::Unicode,
        Some(_) => Charset::Ascii,
    }
}

/// The usable line width for rules, alignment and progress bars.
///
/// `COLUMNS` wins when it parses, because a user who exports it means it.
/// Otherwise the terminal is asked, and a redirected stream falls back to the
/// classic 80. The result is clamped so neither a 20-column phone terminal nor
/// a 400-column ultrawide produces unreadable output.
pub fn width(env: &Env, terminal: Option<u16>) -> usize {
    const FALLBACK: usize = 80;
    const MIN: usize = 40;
    const MAX: usize = 120;
    let raw = env
        .columns
        .as_deref()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .or_else(|| terminal.map(usize::from).filter(|v| *v > 0))
        .unwrap_or(FALLBACK);
    raw.clamp(MIN, MAX)
}
