//! Test Design Specification: the help page a user actually reads.
//!
//! Features tested: that the root page carries usage, every command, both
//! trailing blocks and every variable the binary reads; that `--help` explains more
//! than `-h`; that a subcommand carries its own examples and not the root's;
//! that nothing overruns the width cap; and that every example on the page is
//! an invocation the binary really accepts.
//!
//! Features NOT tested here: the colour policy (owned by `presentation.rs`),
//! which failure gets which code (owned by the contract's own suite and by
//! `presentation.rs`, which asserts the status of a reported failure), and the
//! turn flow (owned by the conformance suite in `tetanus-turn`).
//!
//! Environmental needs: none. `COLUMNS` is stated, never inherited, so the
//! wrapping case measures the same page everywhere.

use std::process::Command;

use tetanus_protocol::rpc::ErrorCode;

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

/// Run the binary and report only what it exited with, for the cases whose
/// whole point is a failing status.
fn status_of(args: &[&str]) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .args(args)
        .env("COLUMNS", "100")
        .env_remove("NO_COLOR")
        .output()
        .expect("the binary runs")
        .status
        .code()
        .expect("it exited rather than being signalled")
}

/// The examples of a page wide enough for two columns, split back into the
/// command and what it is for. What a narrow page has to say the same way.
fn wide_pairs(page: &str) -> Vec<(String, String)> {
    block(page, "Examples:")
        .iter()
        .filter_map(|line| {
            let (command, what) = line.trim().split_once("  ")?;
            Some((command.trim().to_string(), what.trim_start().to_string()))
        })
        .collect()
}

/// The rows of the exit-status block, as the number and the words beside it.
///
/// A folded meaning continues in the number column's own width, so its first
/// word is never a number and the line drops out here - which is what makes
/// this readable at any width the page is asked for.
fn statuses(page: &str) -> Vec<(u8, String)> {
    block(page, "Exit status:")
        .iter()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let status = words.next()?.parse::<u8>().ok()?;
            Some((status, words.collect::<Vec<_>>().join(" ")))
        })
        .collect()
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
    for command in [
        "run", "chat", "config", "sessions", "replay", "serve", "info",
    ] {
        assert!(
            block(&page, "Commands:")
                .iter()
                .any(|line| line.trim_start().starts_with(command)),
            "`{command}` is not listed:\n{page}"
        );
    }
    // A user who cannot make the binary work looks here first, so every
    // variable that changes its behaviour has to be on the page.
    for variable in [
        "DEEPSEEK_API_KEY",
        "NO_COLOR",
        "CLICOLOR_FORCE",
        "CLICOLOR",
        "TERM",
        "LC_ALL",
        "LC_CTYPE",
        "LANG",
        "COLUMNS",
    ] {
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
        vec!["chat", "--help"],
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
    let chat = help(&["chat", "--help"]);
    let examples: Vec<&str> = block(&root, "Examples:")
        .into_iter()
        .chain(block(&run, "Examples:"))
        .chain(block(&chat, "Examples:"))
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
    for (args, count) in [
        (vec!["--help"], 16),
        (vec!["run", "--help"], 9),
        (vec!["chat", "--help"], 5),
    ] {
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

/// TC-CLI-HELP-8: the exit statuses on the long page.
/// Expected: every status `ErrorCode::exit_status` can return is worded, `0`
/// is worded beside them, each is worded once, and the block is on `--help`
/// only. A code added to the contract with a status nobody worded leaves a
/// caller reading a number the page does not explain, and this is the case
/// that finds it.
#[test]
fn every_exit_status_the_contract_defines_is_worded() {
    let page = help(&["--help"]);
    let rows = statuses(&page);
    assert!(!rows.is_empty(), "the block went missing:\n{page}");

    // The codes are walked rather than listed: JSON-RPC keeps its own errors
    // in -32768..-32000 and leaves -32099..-32000 to the server, so a code the
    // contract adds is found here without this test being edited.
    let mut found = 0;
    for raw in -32800..=-31900 {
        let Some(code) = ErrorCode::from_code(raw) else {
            continue;
        };
        found += 1;
        let status = code.exit_status();
        assert!(
            rows.iter().any(|(worded, _)| *worded == status),
            "{code:?} exits {status}, which the page does not word:\n{page}"
        );
    }
    assert!(found >= 15, "only {found} codes were reachable by number");

    assert!(
        rows.iter().any(|(status, _)| *status == 0),
        "nothing says what a run that worked exits with:\n{page}"
    );
    for (status, _) in &rows {
        assert_eq!(
            rows.iter().filter(|(other, _)| other == status).count(),
            1,
            "{status} is worded twice:\n{page}"
        );
    }

    // A status is read by the script around a person; `-h` is the summary
    // that person skims for a flag.
    assert!(
        !help(&["-h"]).contains("Exit status:"),
        "the block is on the short page"
    );
}

/// TC-CLI-HELP-9: the page against the binary.
/// Expected: each of a run that worked, a flag that does not exist and a
/// journal that is not there exits with a status the block words. The wording
/// is judged by a reader; that the number is one the page admits to is not,
/// and a status the page never mentions is the failure this catches.
#[test]
fn the_statuses_the_page_words_are_the_ones_it_exits_with() {
    let page = help(&["--help"]);
    let rows = statuses(&page);

    for (args, expected) in [
        (vec!["info"], 0),
        (vec!["--color", "bogus"], 2),
        (vec!["replay", "no-such-journal.jsonl"], 4),
    ] {
        let status = status_of(&args);
        assert_eq!(status, expected, "`{args:?}` exited {status}");
        assert!(
            rows.iter().any(|(worded, _)| i32::from(*worded) == status),
            "`{args:?}` exits {status}, which the page does not word:\n{page}"
        );
    }
}

/// TC-CLI-HELP-10: the examples in a window with no room for two columns.
/// Expected: on each of the three blocks, at every sampled width under the one
/// its two columns need, every command is on a line of its own and what it is
/// for is under it, indented past it, and nothing else is in the block; and at
/// the width they need, the two columns are back. clap folds a row it is
/// handed that does not fit, and it continues it in the command column - where
/// the rest of a description reads as the start of another command.
///
/// 44 is where the widest example command itself stops fitting. Under that,
/// the `Policy` floor of 40 leaves that one command to clap, which is the only
/// case on the page this layout cannot rescue.
///
/// The width each block needs is measured from the block rather than written
/// down here, because a row added to one moves it: the root block needs 79
/// today and needed 76 before `chat` was on it.
#[test]
fn the_examples_stack_where_two_columns_do_not_fit() {
    for (args, count) in [
        (vec!["--help"], 16),
        (vec!["run", "--help"], 9),
        (vec!["chat", "--help"], 5),
    ] {
        let pairs = wide_pairs(&help_at("100", &args));
        assert_eq!(pairs.len(), count, "the examples went missing");

        let widest = |of: fn(&(String, String)) -> &String| {
            pairs
                .iter()
                .map(|pair| of(pair).chars().count())
                .max()
                .unwrap_or(0)
        };
        let fits = 2 + widest(|pair| &pair.0) + 2 + widest(|pair| &pair.1);

        for columns in ["44", "48", "56", "66", "72"]
            .into_iter()
            .filter(|columns| columns.parse::<usize>().unwrap() < fits)
        {
            let page = help_at(columns, &args);
            let rows = block(&page, "Examples:");

            for (command, what) in &pairs {
                assert!(
                    rows.iter()
                        .any(|line| line.trim_end() == format!("  {command}")),
                    "`{command}` is not on a line of its own at {columns}:\n{page}"
                );
                assert!(
                    rows.iter()
                        .any(|line| line.trim_end() == format!("      {what}")),
                    "`{what}` is not under its command at {columns}:\n{page}"
                );
            }
            for line in &rows {
                assert!(
                    line.starts_with("  tetanus ")
                        || (line.starts_with("      ") && !line[6..].starts_with(' ')),
                    "`{line}` is neither a command nor a description at {columns}"
                );
            }
        }

        // The width the second column fits in again, and the layout every
        // wider window has been reading for as long as the page has existed.
        let page = help_at(&fits.to_string(), &args);
        let back = block(&page, "Examples:");
        assert_eq!(back.len(), count, "{fits} columns did not go back to two");
    }
}

/// TC-CLI-HELP-9: the environment block at a window too narrow for two
/// columns.
/// Expected: nothing is folded at column zero. A variable's name is a row of
/// its own and what it changes is indented under it, so no continuation of a
/// description can be read as another variable's name - which is what a block
/// written out with its own spaces gets from clap, and the reason the two
/// blocks on this page are composed rather than written.
#[test]
fn the_environment_block_folds_under_itself() {
    let page = help_at("40", &["--help"]);
    let block = block(&page, "Environment:");

    assert!(!block.is_empty(), "no environment block:\n{page}");
    for line in &block {
        assert!(line.chars().count() <= 40, "`{line}` overruns 40");
        let indented = line.starts_with("  ");
        assert!(indented, "`{line}` starts at column zero");
    }
    // Each variable is still findable as the start of a row.
    for variable in ["DEEPSEEK_API_KEY", "TERM", "COLUMNS"] {
        assert!(
            block
                .iter()
                .any(|line| line.trim_start().starts_with(variable)),
            "`{variable}` does not start a row:\n{page}"
        );
    }
}

/// TC-CLI-HELP-10: an example wider than the window.
/// Expected: the command folds under itself, indented, rather than at column
/// zero. `tetanus run -a deepseek -m deepseek-v4-pro` at forty columns is the
/// case: its tail at column zero reads as the start of another example.
#[test]
fn a_wide_example_folds_under_itself() {
    let page = help_at("40", &["--help"]);
    let block = block(&page, "Examples:");

    for line in &block {
        assert!(line.chars().count() <= 40, "`{line}` overruns 40");
        assert!(line.starts_with("  "), "`{line}` starts at column zero");
    }
    assert!(
        block.iter().any(|line| line.contains("deepseek-v4-pro")),
        "the folded example lost its tail:\n{page}"
    );
}
