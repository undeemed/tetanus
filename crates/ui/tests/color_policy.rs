//! Test Design Specification: the color, charset and width policy.
//!
//! Features tested: the documented precedence between `--color`, `NO_COLOR`,
//! `CLICOLOR_FORCE`, `TERM=dumb`, `CLICOLOR=0` and terminal detection; the
//! charset choice; the width resolution and its clamp.
//!
//! Features NOT tested here: how a resolved theme renders (see `writer.rs`),
//! and the process-environment read itself - `Env::from_process` is the one
//! uncovered line by design, so no case has to mutate shared process state.
//!
//! Approach: every case states its world as an `Env` value and calls the pure
//! resolver. Pass criterion for each case is the stated expected result.
//!
//! Environmental needs: none.

use tetanus_ui::color::{charset, color_enabled, width, Charset, ColorChoice, Env};

fn env(pairs: &[(&str, &str)]) -> Env {
    let mut env = Env::default();
    for (key, value) in pairs {
        let slot = match *key {
            "NO_COLOR" => &mut env.no_color,
            "CLICOLOR" => &mut env.clicolor,
            "CLICOLOR_FORCE" => &mut env.clicolor_force,
            "TERM" => &mut env.term,
            "LANG" => &mut env.locale,
            "COLUMNS" => &mut env.columns,
            other => panic!("unknown variable in a case: {other}"),
        };
        *slot = Some((*value).to_string());
    }
    env
}

/// TC-UI-COLOR-1: `--color` is this invocation's explicit intent.
/// Expected: `always` and `never` win over every environment variable and over
/// stream detection, in both directions.
#[test]
fn an_explicit_flag_beats_the_environment() {
    let hostile = env(&[("NO_COLOR", "1"), ("TERM", "dumb"), ("CLICOLOR", "0")]);
    assert!(color_enabled(ColorChoice::Always, &hostile, false));

    let friendly = env(&[("CLICOLOR_FORCE", "1"), ("TERM", "xterm-256color")]);
    assert!(!color_enabled(ColorChoice::Never, &friendly, true));
}

/// TC-UI-COLOR-2: `NO_COLOR` on a terminal.
/// Expected: color off, even though the stream is a tty.
#[test]
fn no_color_turns_a_terminal_plain() {
    assert!(!color_enabled(
        ColorChoice::Auto,
        &env(&[("NO_COLOR", "1")]),
        true
    ));
}

/// TC-UI-COLOR-3: `NO_COLOR` set to the empty string.
/// Expected: ignored - the convention is "set to a non-empty value".
#[test]
fn an_empty_no_color_is_not_set() {
    assert!(color_enabled(
        ColorChoice::Auto,
        &env(&[("NO_COLOR", "")]),
        true
    ));
}

/// TC-UI-COLOR-4: `NO_COLOR` and `CLICOLOR_FORCE` both set.
/// Expected: color off. `NO_COLOR` is checked first, matching `anstream`, so
/// tetanus resolves the conflict the same way the rest of the ecosystem does.
#[test]
fn no_color_outranks_clicolor_force() {
    let both = env(&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")]);
    assert!(!color_enabled(ColorChoice::Auto, &both, false));
}

/// TC-UI-COLOR-5: `CLICOLOR_FORCE` while piped.
/// Expected: color on, because forcing is the whole point of the variable;
/// `CLICOLOR_FORCE=0` is not forcing and falls through to detection.
#[test]
fn clicolor_force_colors_a_pipe() {
    assert!(color_enabled(
        ColorChoice::Auto,
        &env(&[("CLICOLOR_FORCE", "1")]),
        false
    ));
    assert!(!color_enabled(
        ColorChoice::Auto,
        &env(&[("CLICOLOR_FORCE", "0")]),
        false
    ));
}

/// TC-UI-COLOR-6: a dumb terminal, and an explicit `CLICOLOR=0`.
/// Expected: color off in both cases despite the stream being a tty.
#[test]
fn a_dumb_terminal_and_clicolor_zero_are_plain() {
    assert!(!color_enabled(
        ColorChoice::Auto,
        &env(&[("TERM", "dumb")]),
        true
    ));
    assert!(!color_enabled(
        ColorChoice::Auto,
        &env(&[("CLICOLOR", "0")]),
        true
    ));
}

/// TC-UI-COLOR-7: nothing set at all.
/// Expected: the stream decides - color on a tty, plain on a pipe.
#[test]
fn a_bare_environment_defers_to_the_stream() {
    let bare = Env::default();
    assert!(color_enabled(ColorChoice::Auto, &bare, true));
    assert!(!color_enabled(ColorChoice::Auto, &bare, false));
}

/// TC-UI-COLOR-8: charset resolution.
/// Expected: UTF-8 locale and no locale draw Unicode; a non-UTF-8 locale and a
/// dumb terminal fall back to ASCII.
#[test]
fn the_charset_follows_the_locale() {
    assert_eq!(charset(&env(&[("LANG", "en_US.UTF-8")])), Charset::Unicode);
    assert_eq!(charset(&Env::default()), Charset::Unicode);
    assert_eq!(charset(&env(&[("LANG", "C")])), Charset::Ascii);
    assert_eq!(
        charset(&env(&[("TERM", "dumb"), ("LANG", "en_US.UTF-8")])),
        Charset::Ascii
    );
}

/// TC-UI-COLOR-9: width resolution.
/// Expected: `COLUMNS` wins when it parses; otherwise the terminal's width;
/// otherwise 80. Every result is clamped into 40..=120.
#[test]
fn width_prefers_columns_then_the_terminal_then_eighty() {
    assert_eq!(width(&env(&[("COLUMNS", "100")]), Some(60)), 100);
    assert_eq!(width(&env(&[("COLUMNS", "not a number")]), Some(60)), 60);
    assert_eq!(width(&Env::default(), Some(60)), 60);
    assert_eq!(width(&Env::default(), None), 80);
    assert_eq!(width(&env(&[("COLUMNS", "10")]), None), 40);
    assert_eq!(width(&Env::default(), Some(400)), 120);
    assert_eq!(width(&env(&[("COLUMNS", "0")]), None), 80);
}
