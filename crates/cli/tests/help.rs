//! Test Design Specification: the help page a user actually reads.
//!
//! Features tested: that the root page carries usage, every command, both
//! trailing blocks and the environment it honours; that `--help` explains more
//! than `-h`; that a subcommand carries its own examples and not the root's;
//! that nothing overruns the width cap; and that every example on the page is
//! an invocation the binary really accepts.
//!
//! Features NOT tested here: the colour policy (owned by `presentation.rs`)
//! and the turn flow (owned by the conformance suite in `tetanus-turn`).
//!
//! Environmental needs: none. `COLUMNS` is stated, never inherited, so the
//! wrapping case measures the same page everywhere.

use std::process::Command;

/// Ask the binary for a help page, at the widest width it uses, into a pipe.
fn help(args: &[&str]) -> String {
    help_at("100", args)
}

/// The same, at a stated terminal width.
fn help_at(columns: &str, args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.args(args)
        .env("COLUMNS", columns)
        .env_remove("NO_COLOR");
    let out = cmd.output().expect("the binary runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// The indented lines under a named block, up to the next blank line.
fn block<'a>(page: &'a str, name: &str) -> Vec<&'a str> {
    page.lines()
        .skip_while(|line| line.trim() != name)
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .collect()
}

/// TC-CLI-HELP-1: `tetanus --help`.
/// Expected: usage, every subcommand, the global flag with its accepted
/// values, and both trailing blocks naming the environment the binary honours.
#[test]
fn the_root_page_is_complete() {
    let page = help(&["--help"]);

    for expected in [
        "Usage: tetanus",
        "Commands:",
        "Options:",
        "--color <WHEN>",
        "[possible values: auto, always, never]",
        "Examples:",
        "Environment:",
    ] {
        assert!(page.contains(expected), "`{expected}` missing:\n{page}");
    }
    for command in ["run", "config", "sessions", "replay", "serve", "info"] {
        assert!(
            block(&page, "Commands:")
                .iter()
                .any(|line| line.trim_start().starts_with(command)),
            "`{command}` is not listed:\n{page}"
        );
    }
    // A user who cannot make the binary work looks here first, so every
    // variable that changes its behaviour has to be on the page.
    for variable in ["DEEPSEEK_API_KEY", "NO_COLOR", "CLICOLOR_FORCE", "COLUMNS"] {
        assert!(
            block(&page, "Environment:")
                .iter()
                .any(|line| line.contains(variable)),
            "`{variable}` is undocumented:\n{page}"
        );
    }
}

/// TC-CLI-HELP-2: `-h` against `--help`.
/// Expected: the short form stays a summary; the long form additionally says
/// what a turn is, which is the one concept the rest of the page assumes.
#[test]
fn the_long_form_explains_the_unit_of_work() {
    let short = help(&["-h"]);
    let long = help(&["--help"]);

    assert!(!short.contains("A turn is the unit of work"), "{short}");
    assert!(long.contains("A turn is the unit of work"), "{long}");
    assert!(long.len() > short.len());
}

/// TC-CLI-HELP-3: `tetanus run --help`.
/// Expected: the subcommand's own examples, and no trace of the root block -
/// an epilogue attached to the wrong command is the usual way this breaks.
#[test]
fn a_subcommand_carries_its_own_examples() {
    let page = help(&["run", "--help"]);

    assert!(page.contains("Examples:"), "{page}");
    assert!(page.contains("--max-steps 1"), "{page}");
    assert!(
        !page.contains("Environment:"),
        "the root epilogue leaked into the subcommand:\n{page}"
    );
}

/// TC-CLI-HELP-4: the page at the stated width.
/// Expected: no line is wider than the cap, epilogues included. A hand-written
/// block is not wrapped by clap, so this is the case that catches a long
/// example nobody re-measured.
#[test]
fn nothing_overruns_the_width_cap() {
    for args in [
        vec!["--help"],
        vec!["run", "--help"],
        vec!["replay", "--help"],
        vec!["models", "--help"],
        vec!["tools", "--help"],
        vec!["sessions", "--help"],
        vec!["serve", "--help"],
    ] {
        for line in help(&args).lines() {
            assert!(
                line.chars().count() <= 100,
                "{} chars in `{args:?}`:\n{line}",
                line.chars().count()
            );
        }
    }
}

/// TC-CLI-HELP-5: every example is a real invocation.
/// Expected: each example names a command the binary has, and every flag it
/// shows appears in that command's own help. This is what stops the examples
/// from rotting the first time a flag is renamed.
#[test]
fn every_example_names_something_that_exists() {
    let root = help(&["--help"]);
    let run = help(&["run", "--help"]);
    let examples: Vec<&str> = block(&root, "Examples:")
        .into_iter()
        .chain(block(&run, "Examples:"))
        .collect();
    assert!(examples.len() >= 11, "the examples went missing:\n{root}");

    for example in examples {
        let mut words = example.split_whitespace();
        assert_eq!(
            words.next(),
            Some("tetanus"),
            "not an invocation: {example}"
        );
        let command = words.next().expect("a subcommand");
        let page = help(&[command, "--help"]);
        for flag in words.filter(|word| word.starts_with('-')) {
            assert!(
                page.contains(flag),
                "`{flag}` in `{example}` is not a flag of `{command}`"
            );
        }
    }
}

/// TC-CLI-HELP-6: every model id an example shows is one the adapter offers.
/// Expected: the id in the live-provider example is in the DeepSeek catalog.
/// An example that names a model the binary would reject teaches the wrong
/// thing, and nothing else on the page would catch it.
#[test]
fn every_example_model_is_offered_by_its_adapter() {
    let catalog = tetanus_turn::llm::deepseek::DeepSeekConfig::default().models;
    let page = help(&["--help"]);
    let mut checked = 0;

    for example in block(&page, "Examples:") {
        let words: Vec<&str> = example.split_whitespace().collect();
        if !words.contains(&"deepseek") {
            continue;
        }
        let Some(at) = words
            .iter()
            .position(|word| *word == "-m" || *word == "--model")
        else {
            continue;
        };
        let model = words[at + 1];
        assert!(
            catalog.iter().any(|known| known == model),
            "`{model}` is not in {catalog:?}"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "the live-provider example went missing:\n{page}"
    );
}

/// TC-CLI-HELP-7: the page in an 80-column terminal, the width a user who has
/// never resized anything is reading at.
/// Expected: every hand-aligned example still holds its description on one
/// line. clap wraps an epilogue that does not fit, and a wrapped example loses
/// the column that made the block scannable in the first place.
#[test]
fn the_examples_survive_an_eighty_column_terminal() {
    for (args, count) in [(vec!["--help"], 11), (vec!["run", "--help"], 9)] {
        let page = help_at("80", &args);
        let examples = block(&page, "Examples:");

        assert_eq!(examples.len(), count, "wrapped in `{args:?}`:\n{page}");
        for line in &examples {
            assert!(
                line.starts_with("  tetanus ") && line.chars().count() <= 80,
                "`{line}` in `{args:?}` does not fit 80 columns"
            );
        }
    }
}
