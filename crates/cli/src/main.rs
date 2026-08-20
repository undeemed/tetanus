//! The `tetanus` binary: run one documented turn headlessly.

mod chat;
mod prompt;
mod render;

use tetanus_protocol::methods::{
    AgentPromptResult, ModelCatalogResult, SessionEventsResult, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types as protocol;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::llm::{deepseek, mock, LlmAdapter};
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnTrace};
use tetanus_ui::{
    tame_line, when_killed, ColorChoice, Flow, Frame, Held, Key, Keys, Page, Policy, Role, Screen,
    Stop, Theme, Tty, Ui, View,
};

use render::help;
use render::live::Live;

#[derive(Parser)]
#[command(
    name = "tetanus",
    version,
    about = "tetanus - rust agent harness. everything deepseek-harness has, but better.",
    long_about = "tetanus - rust agent harness. everything deepseek-harness has, but better.\n\n\
                  A turn is the unit of work: the agent claims your prompt, assembles a prompt \
                  and a tool catalogue, calls a model, runs whatever tools the model asked for, \
                  and stops. Every durable fact lands on an append-only JSONL journal you can \
                  replay afterwards.",
    max_term_width = 100
)]
struct Cli {
    /// When to colour output
    #[arg(
        long,
        value_name = "WHEN",
        global = true,
        default_value = "auto",
        value_parser = clap::builder::PossibleValuesParser::new(ColorChoice::NAMES)
    )]
    color: String,
    /// Settings document to read, instead of the one under the harness home
    ///
    /// The harness home is `$TETANUS_HOME` when that is set and `~/.tetanus`
    /// when it is not, and the document in it is `settings.yaml`. That one is
    /// allowed to be missing; a document named here is not.
    #[arg(long, value_name = "PATH", global = true)]
    settings: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one full turn and print the event sequence it emitted
    Run(RunArgs),
    /// Hold a conversation: one session, a turn per message you type
    Chat(chat::ChatArgs),
    /// Show resolved config with provenance
    Config {
        /// Answer as though `--dir <PATH>` had been given to a subcommand
        /// that takes it, so the layer a flag settles on can be read
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// Print what this build compiles in, reading no document at all:
        /// the answer a machine with nothing configured would give
        #[arg(long, conflicts_with = "dir")]
        defaults: bool,
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long)]
        json: bool,
    },
    /// List model providers, the models they advertise, and what is reachable
    Models {
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long)]
        json: bool,
    },
    /// List the tools an agent can call, and the arguments each one takes
    Tools {
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long)]
        json: bool,
    },
    /// List the session journals this build has written
    Sessions {
        /// Directory the journals live in. Defaults to `sessions.root` in
        /// the settings document, or `sessions` when nothing sets it.
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// Move a cursor down the list on a screen of its own, and read the
        /// journal under it with Enter
        #[arg(long, conflicts_with = "json")]
        ui: bool,
        /// Unfold what the model thought, in whichever journal is opened
        #[arg(long, requires = "ui")]
        think: bool,
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long)]
        json: bool,
    },
    /// Replay a session journal
    Replay {
        /// Path to a JSONL journal a previous run wrote, or the id
        /// `tetanus sessions` printed for it
        #[arg(value_name = "JOURNAL", value_parser = named())]
        path: String,
        /// Directory an id is looked up in. Defaults to `sessions.root` in
        /// the settings document, or `sessions` when nothing sets it.
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// Print one line per journal line, including any the timeline refuses
        #[arg(long)]
        raw: bool,
        /// Play the turn back one event at a time, as it happened
        #[arg(long, conflicts_with = "raw")]
        live: bool,
        /// How much faster than the recorded pace to play. Default 1.
        #[arg(long, value_name = "N", requires = "live", value_parser = playback_speed)]
        speed: Option<f64>,
        /// Print the model's thinking in full, not folded to its first line
        #[arg(long)]
        think: bool,
        /// Read the journal on a screen of its own, scrollable, instead of
        /// printing the whole of it into the scrollback
        #[arg(long, conflicts_with_all = ["raw", "live", "json"])]
        ui: bool,
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long, conflicts_with_all = ["raw", "live"])]
        json: bool,
    },
    /// Host the JSON-RPC protocol on stdin and stdout
    Serve {
        /// Directory the journals this server writes land in. Defaults to
        /// `sessions.root` in the settings document, or `sessions` when
        /// nothing sets it.
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// Serve the WebSocket carrier on this address instead of on stdio
        #[arg(long, value_name = "ADDR", value_parser = named())]
        listen: Option<String>,
    },
    /// Print version/build info
    Info,
}

#[derive(clap::Args)]
struct RunArgs {
    /// What to ask the agent. `-` reads it from standard input.
    #[arg(value_name = "PROMPT")]
    ask: Option<String>,
    /// What to ask the agent, named rather than positional
    #[arg(short, long, value_name = "TEXT", conflicts_with = "ask")]
    prompt: Option<String>,
    /// Which model provider to resolve into the registry. Defaults to
    /// `provider.default` in the settings document, and to the mock adapter
    /// when nothing sets it
    #[arg(short, long, value_enum)]
    adapter: Option<AdapterChoice>,
    /// Model id. Defaults to the adapter's first catalog entry.
    #[arg(short, long, value_name = "ID", value_parser = named())]
    model: Option<String>,
    /// Where the session journal lands. Defaults to `turn.jsonl` under
    /// `sessions.root` in the settings document
    #[arg(short, long, value_name = "PATH")]
    session: Option<PathBuf>,
    /// Step budget for the turn. Defaults to `agent.max_steps` in the
    /// settings document
    #[arg(long, value_name = "N")]
    max_steps: Option<u32>,
    /// Print the raw event sequence instead of the turn
    #[arg(long)]
    trace: bool,
    /// With `--trace`, print each event's payload next to its topic
    #[arg(long)]
    verbose: bool,
    /// Print the model's thinking in full, not folded to its first line
    #[arg(long)]
    think: bool,
    /// Print the events and the summary as JSON, per contract §4.7
    #[arg(long, conflicts_with = "trace")]
    json: bool,
    /// Watch the turn on a screen of its own, scrollable, instead of in a
    /// block under the shell prompt
    #[arg(long, conflicts_with_all = ["trace", "json"])]
    ui: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AdapterChoice {
    /// Deterministic built-in adapter. No key, no network.
    Mock,
    /// DeepSeek chat completions. Needs `DEEPSEEK_API_KEY`.
    Deepseek,
}

impl AdapterChoice {
    /// The provider route this choice names. `tetanus models` prints these,
    /// and `--adapter` accepts them, so what a user reads is what they type.
    fn route(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Deepseek => "deepseek",
        }
    }
}

/// A failure already reported to the user, carrying the status to exit with.
///
/// Diagnostics go through the writer like every other line, so `main` reports
/// nothing itself: by the time this is returned, the message is already on
/// stderr in the right palette.
struct Reported(u8);

fn main() -> std::process::ExitCode {
    // Help is rendered during parsing, so the palette is decided from raw argv
    // before clap sees it. See `render::help`.
    let argv: Vec<String> = std::env::args().collect();
    let policy = Policy::from_process(help::color_from_argv(argv.iter().skip(1)));

    let cli = Cli::parse_from_command(&policy);

    // The pre-scan is lenient by design; clap's parse is the strict one, so
    // the flag it validated is what governs the output that follows.
    let policy = Policy::from_process(color_choice(&cli.color));
    match run_command(&policy, cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(Reported(status)) => std::process::ExitCode::from(status),
    }
}

impl Cli {
    /// Build the command with the resolved palette attached, then parse.
    ///
    /// clap exits the process itself on `--help`, `--version` and a usage
    /// error, which is why this returns the parsed value rather than a result.
    fn parse_from_command(policy: &Policy) -> Self {
        let theme = policy.stdout;
        let command = <Self as clap::CommandFactory>::command()
            .color(help::command_style(theme.color()))
            // The page folds where every other page this binary prints folds.
            // clap would otherwise measure the terminal a second time and
            // reach a different answer at the two ends of the clamp, which is
            // where a block folded here would be folded again.
            .term_width(policy.width)
            .styles(help::styles())
            .after_help(help::root_epilogue(&theme, policy.width))
            .after_long_help(help::root_long_epilogue(&theme, policy.width))
            .mut_subcommand("run", |run| {
                run.after_help(help::run_epilogue(&theme, policy.width))
            })
            .mut_subcommand("chat", |chat| {
                chat.after_help(help::chat_epilogue(&theme, policy.width))
            });
        <Self as clap::FromArgMatches>::from_arg_matches(&command.get_matches())
            .unwrap_or_else(|err| err.exit())
    }
}

/// Refuse a value that names nothing.
///
/// Every flag that takes a path takes a `PathBuf`, and clap already refuses an
/// empty one. The three values that stay text - a model id, the journal
/// `replay` reads, and the address `serve` binds - had no such rule, and each
/// carried the empty string somewhere further on: a run announced itself on a
/// model with no name, `replay` reported a journal missing when none had been
/// named, and `serve` said `: invalid socket address`. This is clap's own
/// rule, so every one of them now refuses the same mistake in the same words,
/// with the exit §4.5 gives a bad argument.
///
/// Only the empty string. A name made of spaces is a name this build cannot
/// judge - a file may be called that - and refusing it would be this module
/// deciding what a path may be.
fn named() -> clap::builder::NonEmptyStringValueParser {
    clap::builder::NonEmptyStringValueParser::new()
}

/// Accept a playback speed, rejecting the values the arithmetic cannot use.
///
/// Zero, a negative and a NaN all make a duration that cannot be waited for,
/// so they are usage errors caught by clap rather than a panic mid-playback.
fn playback_speed(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(speed) if speed.is_finite() && speed > 0.0 => Ok(speed),
        Ok(_) => Err("expected a number greater than zero".into()),
        Err(err) => Err(err.to_string()),
    }
}

/// Map the validated flag value onto the policy's choice. Unreachable
/// otherwise: clap rejected anything not in [`ColorChoice::NAMES`].
fn color_choice(value: &str) -> ColorChoice {
    match value {
        "always" => ColorChoice::Always,
        "never" => ColorChoice::Never,
        _ => ColorChoice::Auto,
    }
}

fn run_command(policy: &Policy, cli: Cli) -> Result<(), Reported> {
    let mut out = policy.stdout();
    // Which document every boot below reads, settled once and for all of
    // them. `--settings` is global, so a path that names nothing is the same
    // mistake whichever subcommand it was typed at, and one answer here is
    // what keeps `tetanus config` describing the document the next command
    // will read.
    let document = document(policy, cli.settings)?;
    match cli.cmd {
        Cmd::Run(args) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            runtime.block_on(run(policy, &document, &mut out, args))
        }
        Cmd::Chat(args) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            let held = runtime.block_on(chat::chat(policy, &document, &mut out, args));
            // The line reader is a blocking read that nothing can cancel, so a
            // chat left with Ctrl-C exits with one still parked on standard
            // input. Dropping the runtime waits for its pool; this does not,
            // and the process is on its way out either way.
            runtime.shutdown_background();
            held
        }
        Cmd::Config {
            dir,
            defaults,
            json,
        } => {
            // What the engine would run on, not what this surface believes:
            // the same boot every other subcommand does, so the page reports
            // the keys the engine has and the layer each came from.
            //
            // A flag is only on the `Flag` layer of the process it was typed
            // at, so a page with no flags can never print `flag` - and the
            // flag layer is exactly what a reader comes to this command to
            // understand. `--dir` is that flag, asked here as a question
            // rather than as an instruction: it lists nothing and opens
            // nothing, it says what a subcommand given it would run on.
            //
            // The page is the engine's own `config.dump`, not a copy of the
            // resolved layers: the engine reports the value it will actually
            // use for a key it settles, and it drops the value of a key whose
            // name says it holds a credential (contract §4.3). A surface that
            // printed the layers itself would print that credential.
            //
            // `--defaults` asks the other question a reader has here: not
            // "what is set", but "what does this build settle when nothing
            // is". It is the page with the document left out, which is also
            // the page a document that cannot be read still has - and that is
            // when the question is asked most.
            let (settings, read) = match defaults {
                true => (compiled(policy)?, None),
                false => (
                    booted(policy, &document, &root(dir.as_deref()))?,
                    Some(place(&document)),
                ),
            };
            let engine = tetanus_engine::HarnessEngine::new(settings);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            let dump = runtime
                .block_on(tetanus_protocol::methods::Engine::config_dump(&engine))
                .map_err(|err| fail(policy, &err))?;
            if json {
                render::json::line(&mut out, &dump)
                    .map_err(|err| report(policy, &err.to_string(), None))?;
            } else {
                render::config::render(&mut out, &dump.entries, read.as_deref()).ok();
            }
            if defaults {
                // A page that is not what the harness will run on has to say
                // so, in both views and on neither's stream: every row saying
                // `default` is the same page a machine with no document has,
                // and a reader who came here because theirs is not working
                // would read it as proof that it is.
                policy
                    .stderr()
                    .note("what this build compiles in, not what it will run on")
                    .ok();
            }
            Ok(())
        }
        Cmd::Models { json } => {
            let catalog = providers();
            if json {
                return render::json::line(&mut out, &catalog)
                    .map_err(|err| report(policy, &err.to_string(), None));
            }
            render::catalog::models(&mut out, &catalog).ok();
            Ok(())
        }
        Cmd::Tools { json } => {
            let catalog = catalog();
            if json {
                return render::json::line(&mut out, &catalog)
                    .map_err(|err| report(policy, &err.to_string(), None));
            }
            render::catalog::tools(&mut out, &catalog).ok();
            Ok(())
        }
        Cmd::Sessions {
            dir,
            ui,
            think,
            json,
        } => {
            // Answered before the directory is read, for the reason `run` and
            // `replay` answer it before a journal is opened: a flag the
            // terminal cannot honour is wrong at the moment it is read.
            if ui && !policy.stdout_is_terminal {
                return Err(fail(policy, &nowhere_to_draw()));
            }
            // A listing is the store's own view of a directory: what ids it
            // holds, and which of them a turn is running on. No surface can
            // assemble that from a path, which is why this is the first
            // subcommand whose whole answer comes from the engine.
            //
            // Which directory is the settings document's answer unless the
            // flag overrode it, and the engine is built from the whole of
            // that boot rather than from a path and the compiled defaults:
            // a listing under one set of settings and a run under another
            // would be two harnesses wearing one name.
            let settings = booted(policy, &document, &root(dir.as_deref()))?;
            // Read off the settled settings rather than off `--dir`, so the
            // page names the directory that was actually listed even when
            // nobody passed a flag and the document chose it.
            let under = place(&settings.sessions_root);
            let engine = tetanus_engine::HarnessEngine::new(settings);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            let list = runtime
                .block_on(tetanus_protocol::methods::Engine::session_list(&engine))
                .map_err(|err| fail(policy, &err))?;
            if json {
                return render::json::line(&mut out, &list)
                    .map_err(|err| report(policy, &err.to_string(), None));
            }
            if ui {
                // The one place this binary reads a journal is `replay`, and
                // this is that same read: the same reader, the same crossing of
                // the boundary, and the same wording for every way it fails.
                let open = |path: &str| {
                    tetanus_session::replay(path)
                        .map(boundary)
                        .map_err(|err| journal_fault(&err, std::path::Path::new(path)).message)
                };
                return match render::pick::pick(&mut out, &list, &under, think, &open) {
                    Ok(Stop::Interrupted) => Err(stopped(policy)),
                    Ok(Stop::Quit) => Ok(()),
                    // The list is worth more than the view of it, and by here
                    // the terminal has been given back to print it on.
                    Err(err) => {
                        policy
                            .stderr()
                            .note(&format!("no full-screen view: {err}"))
                            .ok();
                        render::sessions::render(&mut out, &list, &under).ok();
                        Ok(())
                    }
                };
            }
            render::sessions::render(&mut out, &list, &under).ok();
            Ok(())
        }
        Cmd::Replay {
            path,
            dir,
            raw,
            live,
            speed,
            think,
            ui,
            json,
        } => {
            // Before the path is even looked at, for the reason `run` answers
            // it before the journal is opened: a flag the terminal cannot
            // honour is wrong at the moment it is read.
            if ui && !policy.stdout_is_terminal {
                return Err(fail(policy, &nowhere_to_draw()));
            }
            // A target that is nothing at all is a typo, and reading it as
            // an empty session is how a typo becomes a blank page and a zero
            // exit. The lookup is here, before any view is chosen, so every
            // shape of `replay` finds and fails the same way.
            let path = journal_named(policy, &document, &path, dir.as_deref())?;
            // `--raw` is the view for a journal the reader below refuses,
            // so it opens the file itself. Asking for a log first would make
            // the one command that reads a broken journal fail on exactly the
            // journals it exists for.
            if raw {
                let lines = journal_lines(&path).map_err(|err| {
                    fail(policy, &journal_fault(&err, std::path::Path::new(&path)))
                })?;
                render::raw::render(&mut out, &lines).ok();
                return match render::raw::unreadable(&lines) {
                    None => Ok(()),
                    Some(line) => {
                        let mut err = policy.stderr();
                        err.error(&render::fault::corrupt_at(line as u64)).ok();
                        err.note("that line is shown above; repair or remove it")
                            .ok();
                        Err(Reported(ErrorCode::LogCorrupt.exit_status()))
                    }
                };
            }
            let events = tetanus_session::replay(&path)
                .map_err(|err| fail(policy, &journal_fault(&err, std::path::Path::new(&path))))?;
            let events = boundary(events);
            if json {
                // `session.events` answers with one page. A journal read from
                // disk is the whole of it, so the page is too.
                let page = SessionEventsResult {
                    next_seq: events.last().map(|event| event.seq + 1).unwrap_or_default(),
                    eof: true,
                    events,
                };
                render::json::line(&mut out, &page)
                    .map_err(|err| report(policy, &err.to_string(), None))?;
                return Ok(());
            }
            if ui {
                return match render::browse::browse(&mut out, &path, &events, think) {
                    // Ctrl-C is the one way out that is not "I have read it",
                    // and §4.5 gives an interrupted command 130 - the same
                    // status `--live` reports when it is stopped part way.
                    Ok(Stop::Interrupted) => Err(stopped(policy)),
                    Ok(Stop::Quit) => Ok(()),
                    // The journal is worth more than the view of it. Whatever
                    // the terminal did, the reader still asked to read a turn,
                    // and by here they have their terminal back to read it in.
                    Err(err) => {
                        policy
                            .stderr()
                            .note(&format!("no full-screen view: {err}"))
                            .ok();
                        render::timeline::render(&mut out, &events, think).ok();
                        Ok(())
                    }
                };
            }
            if !live {
                render::timeline::render(&mut out, &events, think).ok();
                return Ok(());
            }
            // One thread is enough: the playback is a clock and a writer.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            let played = runtime.block_on(render::replay::play(
                &mut out,
                policy.stdout_is_terminal,
                &events,
                speed.unwrap_or(1.0),
                think,
                interrupt(),
            ));
            match played {
                Ok(render::replay::Ended::Interrupted) => Err(stopped(policy)),
                _ => Ok(()),
            }
        }
        Cmd::Serve { dir, listen } => {
            // The one subcommand that writes no page: on stdio, stdout belongs
            // to the carrier from here on (contract §4.1), so everything a
            // person reads goes to stderr and `out` is left untouched. The
            // WebSocket carrier does not touch stdout either way, and reads
            // the same page in the same place.
            let mut err = policy.stderr();
            // Before anything is bound or announced. A document the harness
            // cannot read is a fault to report, not a server to start on the
            // defaults: the sessions the caller asked for would land
            // somewhere else, and the banner would say so too late to matter.
            let settings = booted(policy, &document, &root(dir.as_deref()))?;
            // Multi-threaded, because both carriers' properties are
            // concurrency properties: `agent.interrupt` is answered while the
            // prompt it interrupts still runs, and a push overtakes the answer
            // of a call in flight. A current-thread runtime serves frames one
            // at a time and quietly loses both.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            // Bound before the banner, for two reasons: an address the
            // operating system chose is not the address that was asked for
            // and the banner has to print the real one, and a bind that fails
            // must not have announced a server first.
            let listener = match &listen {
                Some(address) => Some(
                    runtime
                        .block_on(tokio::net::TcpListener::bind(address))
                        // The address goes in the message rather than in
                        // `data`: §4.5 gives `Io` a `path` field "when a path
                        // is at fault", and an address is not one.
                        .map_err(|refused| {
                            fail(
                                policy,
                                &RpcError::new(ErrorCode::Io, format!("{address}: {refused}")),
                            )
                        })?,
                ),
                None => None,
            };
            let bound = match &listener {
                Some(listener) => Some(
                    listener
                        .local_addr()
                        .map_err(|err| {
                            fail(policy, &RpcError::new(ErrorCode::Io, err.to_string()))
                        })?
                        .to_string(),
                ),
                None => None,
            };
            let carrier = match &bound {
                Some(address) => render::serve::Carrier::WebSocket(address),
                None => render::serve::Carrier::Stdio,
            };
            render::serve::banner(
                &mut err,
                &render::serve::Serving {
                    carrier,
                    sessions: &settings.sessions_root,
                    protocol: tetanus_protocol::PROTOCOL_VERSION,
                },
            )
            .ok();
            let engine: Arc<dyn tetanus_protocol::methods::Engine> =
                Arc::new(tetanus_engine::HarnessEngine::new(settings));
            let served = match listener {
                // A WebSocket server has no end of its own: it accepts until
                // the accept fails, so the interrupt is the shutdown and not
                // an abort. That is why it exits 0 and `tetanus run` exits
                // 130 for the same key - there the interrupt cancels work in
                // progress, here it is the key the banner told the user to
                // press.
                Some(listener) => runtime.block_on(async {
                    tokio::select! {
                        served = tetanus_rpc::websocket::serve(engine, listener) => served,
                        _ = tokio::signal::ctrl_c() => Ok(()),
                    }
                }),
                None => runtime.block_on(tetanus_rpc::stdio::serve(
                    engine,
                    tokio::io::stdin(),
                    tokio::io::stdout(),
                )),
            };
            served.map_err(|broken| {
                fail(policy, &RpcError::new(ErrorCode::Io, broken.to_string()))
            })?;
            render::serve::stopped(&mut err, carrier).ok();
            Ok(())
        }
        Cmd::Info => {
            // Counted from the same two functions the catalogue pages print,
            // so the number here and the list there cannot disagree.
            let build = render::info::Build {
                version: env!("CARGO_PKG_VERSION"),
                protocol: tetanus_protocol::PROTOCOL_VERSION,
                providers: providers().providers.len(),
                tools: catalog().tools.len(),
                os: std::env::consts::OS,
                arch: std::env::consts::ARCH,
            };
            render::info::render(&mut out, &build).ok();
            Ok(())
        }
    }
}

/// Carry journal events across to the contract shape the renderer reads.
///
/// The one crossing left between an engine type and a contract type. The two
/// structs agree field for field, so this is a copy, not a translation: the
/// journal on disk is already the wire shape. It goes when the engine serves
/// `session.events` over the contract itself, and nothing in `render` changes
/// when it does - that is the point of the boundary.
fn boundary(events: Vec<tetanus_session::SessionEvent>) -> Vec<protocol::SessionEvent> {
    events.iter().map(crossing).collect()
}

/// One event across the same boundary.
fn crossing(event: &tetanus_session::SessionEvent) -> protocol::SessionEvent {
    protocol::SessionEvent {
        ty: event.ty.clone(),
        seq: event.seq,
        time: event.time,
        data: event.data.clone(),
        source_event_seqs: event.source_event_seqs.clone(),
    }
}

/// Carry a stop reason across. The one crossing where neither enum contains
/// the other: the contract names reasons only a served call can produce, and
/// the engine names `MaxTokens` and `Interrupted`, which the contract carries
/// as values of the growable `StopReason` rather than as variants.
///
/// The match has no wildcard arm on purpose. A reason the engine adds stops
/// this crate from compiling until someone decides how it crosses, which is
/// the cheapest moment to decide it.
fn reason(reason: tetanus_turn::StopReason) -> protocol::StopReason {
    match reason {
        tetanus_turn::StopReason::Natural => protocol::StopReason::Natural,
        tetanus_turn::StopReason::PreStepRejected => protocol::StopReason::PreStepRejected,
        tetanus_turn::StopReason::MaxSteps => protocol::StopReason::MaxSteps,
        tetanus_turn::StopReason::Cancelled => protocol::StopReason::Cancelled,
        // Neither reason is one the contract names as a variant. Section 7.5
        // makes both values of the growable enum and fixes what a surface does
        // with one, so each crosses as `Other` carrying the engine's own word
        // for it - the same word the journal holds, and the one the timeline
        // then prints.
        //
        // `MaxTokens` is the provider stopping the completion at its output
        // cap, so the answer is unfinished (§4.4.2). `Interrupted` is written
        // by crash repair when a later run finds a journal left open (§4.4.4),
        // never by a turn this process ran.
        tetanus_turn::StopReason::MaxTokens | tetanus_turn::StopReason::Interrupted => {
            protocol::StopReason::Other(reason.as_str().to_string())
        }
    }
}

/// Every provider this build registers, in the contract's shape.
///
/// One list, read by `tetanus models` and by the turn that picks a default
/// model, so the page cannot advertise a catalog the run does not use. It
/// answers `catalog.models` and moves behind the engine when that call is
/// served; nothing in `render` changes when it does.
///
/// Availability is read here and not cached: a user who exports the key and
/// runs the command again must see the answer change.
fn providers() -> ModelCatalogResult {
    let deepseek = deepseek::DeepSeekConfig::default();
    let keyed = !std::env::var(&deepseek.api_key_env)
        .unwrap_or_default()
        .is_empty();
    ModelCatalogResult {
        providers: vec![
            protocol::ProviderDescriptor {
                provider: AdapterChoice::Mock.route().into(),
                models: vec![mock::MODEL.into()],
                credential_env: None,
                available: true,
            },
            protocol::ProviderDescriptor {
                provider: AdapterChoice::Deepseek.route().into(),
                models: deepseek.models.clone(),
                credential_env: Some(deepseek.api_key_env.clone()),
                available: keyed,
            },
        ],
    }
}

/// What one adapter advertises, taken from the list above rather than from a
/// second copy of it.
fn advertised(choice: AdapterChoice) -> Vec<String> {
    providers()
        .providers
        .into_iter()
        .find(|provider| provider.provider == choice.route())
        .map(|provider| provider.models)
        .unwrap_or_default()
}

/// Resolve a named adapter and the model it will run, or report why it cannot.
///
/// Both commands that run a turn ask this, so a credential that is missing
/// fails the same way and a model that was not named defaults the same way,
/// whichever of them was typed. Everything it can refuse is refused before a
/// journal is opened: a chat that cannot reach a model must not first write a
/// session holding no turns.
fn adapter(
    policy: &Policy,
    choice: AdapterChoice,
    model: Option<String>,
) -> Result<(Arc<dyn LlmAdapter>, String), Reported> {
    let adapter: Arc<dyn LlmAdapter> = match choice {
        AdapterChoice::Mock => Arc::new(mock::MockAdapter::new()),
        AdapterChoice::Deepseek => {
            let config = deepseek::DeepSeekConfig::default();
            if std::env::var(&config.api_key_env)
                .unwrap_or_default()
                .is_empty()
            {
                let missing = RpcError::new(
                    ErrorCode::MissingCredential,
                    format!("{} is not set", config.api_key_env),
                )
                .with_data(serde_json::json!({
                    "provider": choice.route(),
                    "env": config.api_key_env,
                }));
                return Err(fail(policy, &missing));
            }
            Arc::new(deepseek::DeepSeekAdapter::with_http(config))
        }
    };
    let model = match model.or_else(|| advertised(choice).first().cloned()) {
        Some(model) => model,
        None => {
            let unusable = RpcError::new(
                ErrorCode::InvalidParams,
                "the adapter advertises no models, so there is nothing to default to",
            )
            .with_data(serde_json::json!({ "field": "model" }));
            return Err(fail(policy, &unusable));
        }
    };
    Ok((adapter, model))
}

/// The tools an agent may call. Built from the registry a turn is booted with,
/// so `tetanus tools` cannot list a tool a run does not have. It answers
/// `catalog.tools`.
fn catalog() -> ToolCatalogResult {
    ToolCatalogResult {
        tools: registry()
            .schemas()
            .into_iter()
            .map(|schema| protocol::ToolDescriptor {
                name: schema.name,
                description: schema.description,
                parameters: schema.parameters,
            })
            .collect(),
    }
}

/// The one registry, so what is listed and what is callable are one thing.
fn registry() -> ToolRegistry {
    ToolRegistry::new().with(Arc::new(EchoTool))
}

/// Ctrl-C, as a future that resolves when it arrives.
///
/// Only the two surfaces that draw a block in place wait on this. Everywhere
/// else the default disposition stands, which is what a user expects from a
/// program that is only printing.
async fn interrupt() {
    tokio::signal::ctrl_c().await.ok();
}

/// Report a run the user stopped, with the status a shell expects from one:
/// 128 plus SIGINT. Not an error - the user asked for this - so it is a
/// warning, and whatever was written before it stands.
fn stopped(policy: &Policy) -> Reported {
    // A warning and not an error: the user asked for this. Only the status
    // comes from the contract, where an interrupted call is `Cancelled`.
    policy.stderr().warn("interrupted").ok();
    Reported(ErrorCode::Cancelled.exit_status())
}

/// Write a diagnostic, with an optional next step, and pick an exit status.
fn report(policy: &Policy, message: &str, hint: Option<&str>) -> Reported {
    let mut err = policy.stderr();
    err.error(message).ok();
    if let Some(hint) = hint {
        err.note(hint).ok();
    }
    // A failure with no contract code behind it is this build's own: §4.5
    // gives `Internal` the status, and nothing here invents another.
    Reported(ErrorCode::Internal.exit_status())
}

/// The same, for a failure that does carry a contract code.
///
/// The wording is `render::fault`'s and the status is the contract's table,
/// so a script can branch on `$?` and read the same number from any tetanus
/// surface (§4.5).
fn fail(policy: &Policy, error: &RpcError) -> Reported {
    let (message, hint) = render::fault::wording(error);
    let mut err = policy.stderr();
    err.error(&message).ok();
    if let Some(hint) = hint {
        err.note(&hint).ok();
    }
    Reported(render::fault::status(error))
}

/// [`fail`], for a settings document the harness cannot run on.
///
/// The wording and the status are the same, because the fault crossed the
/// boundary like any other. The next step is not: `wording` sends a refused
/// value to `--help`, and nothing in a document is a flag. The file is where
/// the value was written and where it has to be fixed, so the note names it -
/// unless the sentence above has named it already, which is the case for
/// every fault about the file itself rather than a value in it.
fn misconfigured(policy: &Policy, document: &std::path::Path, error: &RpcError) -> Reported {
    let (message, _) = render::fault::wording(error);
    let document = document.display().to_string();
    let mut err = policy.stderr();
    err.error(&message).ok();
    if !message.contains(&document) {
        err.note(&format!("{document} is the document that sets it"))
            .ok();
    }
    Reported(render::fault::status(error))
}
/// The settings the harness will run on: the document under the harness home,
/// with whatever the user typed on top of it.
///
/// Every subcommand that builds an engine comes through here, so `tetanus
/// config` describes the settings the next command will actually use. A
/// subcommand that resolved its own would be a second answer to the question
/// this binary has one command for.
///
/// `flags` are the keys the user set on the command line, on the layer that
/// says so: `Flag` outranks `File`, which is what makes a flag win, and
/// `config.dump` then reports the value as `flag`. A flag the user did not
/// pass is not in the list at all - a clap default put on this layer would be
/// a document that could never win.
///
/// Neither step falls back on failure. A document the harness ignored leaves
/// the user configured on paper and unconfigured in fact.
fn booted(
    policy: &Policy,
    document: &std::path::Path,
    flags: &[(&'static str, serde_json::Value)],
) -> Result<tetanus_engine::EngineConfig, Reported> {
    let mut settings = tetanus_engine::boot::document(document).map_err(|err| {
        misconfigured(
            policy,
            document,
            &tetanus_engine::convert::config_error(&err),
        )
    })?;
    for (key, value) in flags {
        settings.set(key, value.clone(), tetanus_config::Layer::Flag);
    }
    tetanus_engine::EngineConfig::from_settings(settings).map_err(|err| {
        let fault = tetanus_engine::convert::config_error(&err);
        // Whoever set the value is who the report is for. A value the flags
        // put there is a flag to retype, and `fail` sends the reader to
        // `--help` as it does for any other bad argument; everything else
        // came off the document, and is fixed by editing it.
        let blamed = fault
            .data
            .as_ref()
            .and_then(|data| data.get("field"))
            .and_then(serde_json::Value::as_str);
        match blamed.is_some_and(|key| flags.iter().any(|(set, _)| *set == key)) {
            true => fail(policy, &fault),
            false => misconfigured(policy, document, &fault),
        }
    })
}

/// The compiled defaults alone: no document, no environment, no flags.
///
/// The same layer [`booted`] starts from, without the layers that answer for a
/// machine. Building it here rather than reading a document is the whole
/// point of `tetanus config --defaults`: the page is then an answer about the
/// build, which is the same for everyone running this binary, and it is still
/// there when the document that would have covered it cannot be read.
fn compiled(policy: &Policy) -> Result<tetanus_engine::EngineConfig, Reported> {
    let mut settings = tetanus_config::Config::default();
    settings.load(
        tetanus_config::Layer::Default,
        tetanus_engine::boot::defaults(),
    );
    // Unreachable while the defaults are the engine's own, and reported rather
    // than unwrapped because a build whose compiled defaults it refuses is a
    // fault to name, not a panic to read a backtrace of.
    tetanus_engine::EngineConfig::from_settings(settings)
        .map_err(|err| fail(policy, &tetanus_engine::convert::config_error(&err)))
}

/// `--dir` as the one settings key it overrides, or nothing when it was not
/// passed. Nothing is the point: it is what leaves the document able to win.
fn root(dir: Option<&std::path::Path>) -> Vec<(&'static str, serde_json::Value)> {
    dir.map(|dir| {
        (
            tetanus_engine::catalog::key::SESSIONS_ROOT,
            serde_json::json!(dir.display().to_string()),
        )
    })
    .into_iter()
    .collect()
}

/// A path as a page names it: absolute, tamed, and marked when nothing is
/// there yet.
///
/// A page that says where it read from is answering "where do I change it",
/// and a relative path only answers that from the directory this run started
/// in - which the page does not print. So the path is made absolute first,
/// without asking the filesystem to resolve it, because a path that is not
/// there yet still has to be named.
///
/// It is tamed for the reason every other path this binary draws is: it can
/// come off a document, an environment or a flag, and none of the three is
/// ours to trust with the cursor.
///
/// A path with nothing at it is still the right answer - it is the file to
/// write, or the directory the first run will make - so it is named and
/// marked rather than left out. The mark matters most on the two pages that
/// have it: a config page of nothing but `default` rows, and a sessions page
/// that lists nothing, both read the same whether the place is empty or the
/// reader is looking at the wrong one.
fn place(path: &std::path::Path) -> String {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let named = tame_line(&full.display().to_string());
    match full.exists() {
        true => named,
        false => format!("{named} (not there yet)"),
    }
}

/// Where this run's settings document lives: the path `--settings` named, or
/// `settings.yaml` under the harness home.
///
/// A document nobody named may be absent. A first run has none, the answer is
/// then the compiled defaults, and every case in the suite would otherwise
/// need one written before the binary would start.
///
/// A document the user named may not. They typed a path because something is
/// in it, and reading the defaults instead would run a harness they did not
/// configure and say nothing about it - the same fault the boot already
/// refuses to fall back on, arriving one step earlier. The reader below
/// reports every other way a document can be wrong, including a path whose
/// extension it cannot parse and a directory where the file should be; a
/// path with nothing at all there is the one it cannot tell from a first run,
/// so it is checked here.
fn document(policy: &Policy, named: Option<PathBuf>) -> Result<PathBuf, Reported> {
    let Some(path) = named else {
        return Ok(tetanus_config::file::document_path(
            &tetanus_config::home::home(None),
        ));
    };
    match path.exists() {
        true => Ok(path),
        false => Err(fail(policy, &missing_document(&path))),
    }
}

/// A settings document the user named that is not there.
///
/// `Io` is §4.5's code for a path the filesystem could not answer for, and it
/// carries the same exit 1 as a document that cannot be parsed: both mean the
/// harness could not be configured the way it was asked to be.
fn missing_document(path: &std::path::Path) -> RpcError {
    RpcError::new(
        ErrorCode::Io,
        format!("no settings document at {}", path.display()),
    )
    .with_data(serde_json::json!({ "path": path.display().to_string() }))
}

/// The flags a command that runs turns was given, before the document has
/// been read. Every one of them is optional for the reason [`root`] gives.
struct TurnFlags {
    adapter: Option<AdapterChoice>,
    model: Option<String>,
    max_steps: Option<u32>,
    session: Option<PathBuf>,
}

/// What a turn will run on, once the document and the flags have both been
/// read.
struct Turn {
    /// The settled settings, so the engine the turn opens on is the one
    /// `tetanus config` described.
    settings: tetanus_engine::EngineConfig,
    provider: AdapterChoice,
    /// `None` leaves the model to the adapter's own catalogue, which is the
    /// only sensible default for an adapter nobody configured.
    model: Option<String>,
    max_steps: u32,
    journal: PathBuf,
}

/// Resolve one turn's settings out of the document and the flags.
///
/// `fallback` is the provider the command has when nobody said otherwise, and
/// it is not always the engine's: `tetanus run` is the mock adapter because a
/// first run must need no credential, and `tetanus chat` is DeepSeek because
/// a conversation with the mock is a demonstration rather than a use. Which
/// is why the compiled default of a key cannot decide this - only a layer
/// somebody wrote can, and that is what the layer is read for below.
fn turn_settings(
    policy: &Policy,
    document: &std::path::Path,
    flags: TurnFlags,
    fallback: AdapterChoice,
    journal: &str,
) -> Result<Turn, Reported> {
    use tetanus_engine::catalog::key;

    let mut overrides: Vec<(&'static str, serde_json::Value)> = Vec::new();
    if let Some(adapter) = flags.adapter {
        overrides.push((key::PROVIDER, serde_json::json!(adapter.route())));
    }
    if let Some(model) = &flags.model {
        overrides.push((key::MODEL, serde_json::json!(model)));
    }
    if let Some(steps) = flags.max_steps {
        overrides.push((key::MAX_STEPS, serde_json::json!(steps)));
    }
    let settings = booted(policy, document, &overrides)?;

    // A key still on its compiled layer is a key nobody has an opinion about,
    // and the command's own default stands. Anything above it - a document, an
    // environment, a flag - is somebody's opinion, and outranks the default
    // this binary happens to compile in.
    let written = |key: &str| {
        settings
            .resolved
            .get(key)
            .is_some_and(|resolved| resolved.layer > tetanus_config::Layer::Default)
    };

    Ok(Turn {
        provider: match written(key::PROVIDER) {
            true => provider_named(policy, document, &settings.default_provider)?,
            false => fallback,
        },
        // Not the settled value when nobody set it: the engine's compiled
        // model belongs to the engine's compiled provider, and offering it to
        // an adapter that never advertised it would name a model that does
        // not exist. An unset model is the adapter's first catalogue entry.
        model: written(key::MODEL).then(|| settings.default_model.clone()),
        max_steps: settings.max_steps,
        journal: flags
            .session
            .unwrap_or_else(|| settings.sessions_root.join(journal)),
        settings,
    })
}

/// The adapter a provider name asks for.
///
/// clap refuses an unknown `--adapter`, so a name that gets this far came out
/// of a document or an environment, and is reported the way any other value
/// in one is: it names the key, and it names the file that has to be edited.
fn provider_named(
    policy: &Policy,
    document: &std::path::Path,
    name: &str,
) -> Result<AdapterChoice, Reported> {
    [AdapterChoice::Mock, AdapterChoice::Deepseek]
        .into_iter()
        .find(|choice| choice.route() == name)
        .ok_or_else(|| {
            let known = [AdapterChoice::Mock, AdapterChoice::Deepseek]
                .map(AdapterChoice::route)
                .join(" or ");
            misconfigured(
                policy,
                document,
                &RpcError::new(
                    ErrorCode::InvalidParams,
                    format!("must be a provider this build can reach, {known}, not {name:?}"),
                )
                .with_data(serde_json::json!({
                    "field": tetanus_engine::catalog::key::PROVIDER,
                })),
            )
        })
}

/// The journal a target names: the path if there is one there, and otherwise
/// the id `tetanus sessions` printed for it, under the root the settings
/// settled.
///
/// The page a reader takes an id off said `turn`, and until this the command
/// they retyped it into answered `no journal at turn` - and sent them to that
/// same page. The two now look in one place. A store resolves an id to
/// `<root>/<id>.jsonl`, so that is what is looked for, and `<root>/<target>`
/// beside it for an id given with the extension it is listed under.
///
/// A path that is there is opened as it was given, and the document is not
/// read at all: a journal the user can see is a journal `replay` opens,
/// whatever a document elsewhere says about roots. Only a target that is
/// nothing on disk asks where the sessions live.
fn journal_named(
    policy: &Policy,
    document: &std::path::Path,
    target: &str,
    dir: Option<&std::path::Path>,
) -> Result<String, Reported> {
    let named = std::path::Path::new(target);
    if named.exists() {
        return Ok(target.to_string());
    }
    let sessions = booted(policy, document, &root(dir))?.sessions_root;
    match [
        sessions.join(target),
        sessions.join(format!("{target}.jsonl")),
    ]
    .into_iter()
    .find(|candidate| candidate.exists())
    {
        Some(found) => Ok(found.display().to_string()),
        None => Err(fail(policy, &missing_journal(named, Some(&sessions)))),
    }
}

/// A journal the user named that is not there, and the root an id was looked
/// for under when the target was not a path.
///
/// The contract's §4.7 mapping sends `tetanus replay <path>` through
/// `session.create`, which creates a journal at a path that has none. That is
/// what `run --session` wants and the opposite of what a read wants, so this
/// surface answers the read before it makes the call.
fn missing_journal(path: &std::path::Path, root: Option<&std::path::Path>) -> RpcError {
    let mut data = serde_json::json!({ "path": path.display().to_string() });
    if let Some(root) = root {
        data["root"] = serde_json::json!(root.display().to_string());
    }
    RpcError::new(
        ErrorCode::SessionNotFound,
        format!("no journal at {}", path.display()),
    )
    .with_data(data)
}

/// `--ui` where there is no screen to draw on.
///
/// Both views that take the flag answer it the same way, at the point the flag
/// was read: it is a bad argument, not a failure of the work the command was
/// asked to do, and §4.5 gives that exit 2.
fn nowhere_to_draw() -> RpcError {
    RpcError::new(ErrorCode::InvalidParams, "--ui needs a terminal to draw on")
        .with_data(serde_json::json!({ "field": "ui" }))
}

/// Read a journal as text, one line at a time.
///
/// Only opening the file can fail here. A line that is not an event is the
/// raw view's to show under its number, not this reader's to refuse - that
/// judgement belongs to `tetanus_session::replay`, which the cooked view
/// uses and this one deliberately does not.
fn journal_lines(path: &str) -> Result<Vec<render::raw::Line>, tetanus_session::SessionError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    Ok(text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(number, line)| render::raw::parse(number + 1, line))
        .collect())
}

/// The same failure, with the file it is about named on it.
///
/// `session.create` is handed a path and reports what the filesystem said
/// about it, but the failure it sends back carries no `path`, so a page can
/// read `is a directory` with no directory on it while `tetanus replay` names
/// the same file on the same mistake. §4.5 asks an `Io` to carry the path
/// when the caller knows one, and here the caller does: it is the file
/// `--session` named. Only that field is filled in - the code, the message
/// and every other field are the engine's and are passed on untouched, and a
/// path the engine did name is never written over.
fn about(mut error: RpcError, path: &std::path::Path) -> RpcError {
    if error.kind() != Some(ErrorCode::Io) {
        return error;
    }
    let data = error.data.get_or_insert_with(|| serde_json::json!({}));
    if let Some(fields) = data.as_object_mut() {
        fields
            .entry("path")
            .or_insert_with(|| serde_json::json!(path.display().to_string()));
    }
    error
}

/// Carry a journal failure across to the contract's error view. A corrupt
/// journal and an unreadable one are different codes, because they are
/// different things for the user to do.
fn journal_fault(error: &tetanus_session::SessionError, path: &std::path::Path) -> RpcError {
    match error {
        tetanus_session::SessionError::Corrupt(line) => {
            RpcError::new(ErrorCode::LogCorrupt, error.to_string())
                .with_data(serde_json::json!({ "line": line }))
        }
        tetanus_session::SessionError::Io(io) => RpcError::new(ErrorCode::Io, io.to_string())
            .with_data(serde_json::json!({ "path": path.display().to_string() })),
        other => RpcError::new(ErrorCode::Internal, other.to_string()),
    }
}

/// Carry a turn failure across to the contract's error view.
///
/// The mapping itself is the engine's, in `tetanus_engine::convert::turn_error`,
/// and contract section 8 names it the only one. This is a call and not a
/// match for two reasons. A second mapping drifts: the code a failure carries
/// is what a script acts on, and two tables deciding it separately is one
/// table too many. And an engine error enum has no fallback variant, so a
/// surface that matched one would stop compiling the day the engine names a
/// new failure - a file this lane owns, broken by a change it has no part in.
///
/// What is left here is the three facts the engine cannot know: which session
/// this was, which provider was routed to, and which file the journal is.
/// Closes #142.
fn turn_fault(
    error: &tetanus_turn::TurnError,
    session_id: &str,
    provider: &str,
    journal: &std::path::Path,
) -> RpcError {
    tetanus_engine::convert::turn_error(session_id, provider, Some(journal), error)
}

/// The status line, for a run whose progress the person cannot already see.
///
/// One rule for the four ways a turn is run, because they used to hold three
/// between them and one of them held none: the plain run asked whether stdout
/// was a terminal, `--json` asked whether stderr was, `--trace` never asked,
/// and `--ui` wrote no status at all. So `tetanus run --json > events 2> log`
/// left `log` empty while the same command with no `--json` filled it.
///
/// `drawn_on_stdout` is whether this way of running shows the turn on stdout:
/// true for the live block and the watched page, false for `--trace`, which
/// prints nothing until the end, and for `--json`, whose stdout is for a
/// script rather than a person.
///
/// The status is then written unless it would be a second spinner on a screen
/// that already has one. A stderr that is not a terminal cannot be: the line
/// degrades to one plain sentence, which is the record a redirected stderr was
/// redirected to keep.
fn status_line(
    policy: &Policy,
    phase: &str,
    drawn_on_stdout: bool,
) -> Option<tetanus_ui::Progress<std::io::Stderr>> {
    if drawn_on_stdout && policy.stderr_is_terminal {
        return None;
    }
    let mut status = policy.stderr_progress();
    status.set(phase).ok();
    Some(status)
}

/// Run `work` behind the status line, ticking the animation while it waits.
///
/// A live model call can hold for a long time with nothing to print, and a
/// surface that prints nothing looks hung. The line is on stderr and is erased
/// before anything else is written, so stdout is unchanged either way.
///
/// Nothing is drawn on stdout here - the trace is printed after the turn - so
/// [`status_line`] always gives one, which is what this did before it asked.
async fn with_progress<F: std::future::Future>(policy: &Policy, label: &str, work: F) -> F::Output {
    let mut progress = status_line(policy, label, false);

    let mut work = std::pin::pin!(work);
    let mut frames = tokio::time::interval(std::time::Duration::from_millis(80));
    let done = loop {
        tokio::select! {
            done = &mut work => break done,
            _ = frames.tick() => {
                if let Some(progress) = &mut progress {
                    progress.tick().ok();
                }
            }
        }
    };
    if let Some(progress) = progress {
        progress.finish().ok();
    }
    done
}

/// Draw the turn while it runs, and commit each settled line as it arrives.
///
/// The events come from the session log the engine is writing, not from the
/// engine: the journal is the durable record, so what a user watches arrive is
/// what a replay will show them tomorrow. Polling it is also what keeps this
/// lane out of the engine - the log is a public type, and the one tracer the
/// conformance suite attaches stays the only observer of the bus.
///
/// The block is on stdout, so [`status_line`] gives a status only when stderr
/// is somewhere else: at one terminal the block's own footer says what a
/// status line would, and two spinners at once read as two programs.
///
/// Returns `None` when the user stopped the turn with Ctrl-C. The turn is
/// dropped where it stands; the block still comes off the screen, and the
/// journal keeps every event that had already been written to it.
///
/// `from` is where on the journal this view starts. A run draws the whole of
/// it, and a chat draws each turn from where that turn began - the journal it
/// appends to already holds the conversation before it, and a view that
/// started at zero would print the whole afternoon again for every question.
async fn with_live<W: std::io::Write, F: std::future::Future>(
    policy: &Policy,
    out: &mut Ui<W>,
    log: &JsonlSessionLog,
    from: usize,
    phase: &str,
    think: bool,
    work: F,
) -> Option<F::Output> {
    let (theme, width) = (*out.theme(), out.width());
    let mut view = Live::new(theme, width, phase, think);
    let mut screen = Screen::new(Ui::new(out.out(), theme, width), policy.stdout_is_terminal);
    let mut status = status_line(policy, phase, policy.stdout_is_terminal);

    let started = std::time::Instant::now();
    let mut seen = from;
    let mut frames = tokio::time::interval(std::time::Duration::from_millis(80));
    let mut work = std::pin::pin!(work);
    let mut stop = std::pin::pin!(interrupt());
    let done = loop {
        tokio::select! {
            done = &mut work => break Some(done),
            _ = &mut stop => break None,
            _ = frames.tick() => {
                seen = settle(&mut view, &mut screen, log, seen);
                // The window can be resized while the turn runs. Asked once,
                // the block would keep drawing at a width that stopped being
                // true, and every frame after the resize would land wrong.
                if policy.stdout_is_terminal {
                    let width = tetanus_ui::measure();
                    screen.resize(width).ok();
                    view.resize(width);
                }
                view.tick();
                screen.draw(&view.block(started.elapsed())).ok();
                if let Some(status) = &mut status {
                    status.tick().ok();
                }
            }
        }
    };

    // Whatever the last frame did not catch. A turn shorter than one frame
    // interval - every offline turn - is committed entirely here.
    settle(&mut view, &mut screen, log, seen);
    screen.finish().ok();
    if let Some(status) = status {
        status.finish().ok();
    }
    done
}

/// What a full-screen watch is of.
///
/// Three values that travel together and are read once each, kept as one so
/// the loop below is a call a reader can take in rather than a row of
/// positional arguments to count off against the signature.
struct Watched<'a> {
    /// What the turn is waiting on before its first event.
    phase: &'a str,
    /// Whether the settled lines unfold what the model thought.
    think: bool,
    /// The right of the heading: the model this turn is running on.
    title: &'a str,
}

/// Watch a turn on a screen of its own.
///
/// The same poll as [`with_live`], drawn as whole frames on the alternate
/// screen instead of as a block on the ordinary one, and reading the keyboard
/// between them so a reader can look back through a turn while it is running.
///
/// The loop is [`tetanus_ui::show`]'s, written again around an async clock
/// rather than reused. `show` waits for a keystroke on the thread it was
/// called on; the turn is a future borrowing the engine, so it can neither be
/// spawned away nor driven from inside a blocking wait. The view itself is a
/// [`View`] all the same, so what it draws and what a key means stay the
/// crate's vocabulary rather than this file's, and the day a turn can be
/// spawned this loop is deleted rather than rewritten.
///
/// The terminal is taken by a [`Held`], so every way out of here gives it back
/// - the turn ending, Ctrl-C, `q`, or a panic on the way through.
///
/// The view outlives the turn. One that came down the instant the model
/// stopped talking would show the finished turn for a single frame, and the
/// reader's chance to look back through what just happened is after it has
/// happened. So the outcome is kept and the loop runs on until the view is
/// closed.
///
/// Which means the view has to say what the outcome was. A turn that finished
/// writes `turn/end` and the page reads it like any other event; a turn that
/// failed writes nothing at all, and the ordinary report of it goes to stderr,
/// behind the alternate screen, where it is read after the reader has given up
/// on a turn they were told nothing about. So a failure is settled onto the
/// page in the wording every other surface gives it, and `fault` is how the
/// caller - which is the only place that knows what kind of error this is -
/// supplies it.
///
/// Returns `None` when the view was closed before the turn finished, however
/// the reader asked: the turn is dropped where it stands and there is no
/// result to report, which contract §4.5 exits `130`. Closing the view over a
/// turn that already finished is not an interruption, and the caller still has
/// its result.
async fn with_page<W, T, E, F>(
    policy: &Policy,
    out: &mut Ui<W>,
    log: &JsonlSessionLog,
    watched: Watched<'_>,
    work: F,
    fault: impl Fn(&E) -> RpcError,
) -> Option<Result<T, E>>
where
    W: std::io::Write,
    F: std::future::Future<Output = Result<T, E>>,
{
    let Watched {
        phase,
        think,
        title,
    } = watched;
    // Raw mode is a property of the process's controlling terminal rather than
    // of a stream, and the alternate screen is two escape codes on the way in
    // and two on the way out. Neither interleaves with the frames `out` paints
    // between them, so the two handles on the one terminal cannot cross.
    // Hung before the terminal is taken rather than after, because the gap
    // between the two is a real one: a signal that lands in it would find the
    // alternate screen already entered and no watch yet to leave it. `held`
    // gives the terminal back on every path out of this function; this gives
    // it back on the paths that are not this function's.
    let killed = when_killed(Tty::new(std::io::stdout())).ok();
    let mut held = match Held::take(Tty::new(std::io::stdout())) {
        Ok(held) => held,
        // It said it was a terminal and then would not be taken. The turn is
        // worth more than the view it was going to be watched in, so it runs
        // in the ordinary block instead and the reason goes to stderr.
        Err((_, err)) => {
            // Nothing was taken, so there is nothing for a signal to give
            // back, and the run that follows may be writing to a pipe.
            drop(killed);
            policy
                .stderr()
                .warn(&format!("{err}; watching in the ordinary view"))
                .ok();
            return with_live(policy, out, log, 0, phase, think, work).await;
        }
    };

    // The page is on stdout, so a stderr that is the same terminal is left
    // alone - it is behind the alternate screen anyway. A stderr that is not
    // gets the one line, which is the whole of what a redirected stderr can
    // learn about a run watched on a screen it cannot see.
    let status = status_line(policy, phase, true);

    let theme = *out.theme();
    let (cols, rows) = tetanus_ui::size();
    let mut watch = Watch {
        log,
        theme,
        live: Live::new(theme, cols, phase, think),
        page: Page::new(theme, "tetanus", title),
        size: (cols, rows),
        seen: 0,
        help: false,
        started: std::time::Instant::now(),
        painted: None,
    };
    // One frame before the loop. Entering the alternate screen and leaving it
    // blank until something happens is a visible flash, and an offline turn
    // finishes inside a frame interval - so without this, the first frame a
    // whole run draws could also be its last.
    watch.paint(out);

    let mut frames = tokio::time::interval(std::time::Duration::from_millis(80));
    let mut work = std::pin::pin!(work);
    let mut stop = std::pin::pin!(interrupt());
    let done = loop {
        tokio::select! {
            done = &mut work => {
                // The view outlives the turn, so it has to be told the turn is
                // over: nothing more will arrive, and from here the spinner
                // would be saying otherwise for as long as the reader watched
                // it. A failure has no event of its own - a turn that fails
                // stops where it stopped - so the wording comes from here too.
                watch.over(out, done.as_ref().err().map(&fault).as_ref());
                break Some(done);
            }
            _ = &mut stop => break None,
            _ = frames.tick() => {
                if watch.beat(out, held.console()) == Flow::Stop {
                    break None;
                }
            }
        }
    };
    if done.is_some() {
        // The turn is over and the view is not. Nothing left here can fail the
        // run, so the only way out is the reader closing the view.
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = frames.tick() => {
                    if watch.beat(out, held.console()) == Flow::Stop {
                        break;
                    }
                }
            }
        }
    }
    held.release().ok();
    if let Some(status) = status {
        status.finish().ok();
    }
    done
}

/// Keystrokes one frame will answer before it draws.
const KEYS: usize = 32;

/// A turn, watched on a screen of its own.
struct Watch<'a> {
    /// The journal the turn is writing, polled rather than subscribed to, for
    /// the reason [`with_live`] gives.
    log: &'a JsonlSessionLog,
    theme: Theme,
    live: Live,
    page: Page,
    /// Columns and rows, as of the last resize the terminal reported.
    size: (usize, usize),
    /// Journal events already settled onto the page.
    seen: usize,
    /// Whether the key card is up in place of the turn.
    help: bool,
    started: std::time::Instant,
    /// The frame the terminal is showing, so an unchanged one is not sent
    /// again. See [`Watch::paint`].
    painted: Option<Frame>,
}

impl Watch<'_> {
    /// The left of the footer, in the longest wording this width has room for.
    /// The short one keeps the card that says the rest, and the way out.
    fn hint(&self, cols: usize) -> String {
        let dot = self.theme.glyph("·", "-");
        render::keys::hint(
            cols,
            &format!(
                "{} scroll {dot} ? keys {dot} q quit",
                self.theme.glyph("↑↓", "up/dn")
            ),
            &format!("? keys {dot} q quit"),
        )
    }

    /// Every key this view answers, in the order a reader meets them.
    ///
    /// The turn's own card. A journal's says how to search, which a turn
    /// arriving has no answer for; this one says how to stop it, which a
    /// journal on disk has no need of.
    fn map(&self) -> Vec<render::keys::Row> {
        vec![
            (
                self.theme.glyph("↑ ↓", "up dn"),
                "one line back, one line on",
            ),
            ("pgup pgdn", "a screenful either way"),
            ("home", "the first line of the turn"),
            ("end", "follow the turn again, as it arrives"),
            ("ctrl-c", "stop the turn, and the view with it"),
            ("q", "close the view; a turn still running is dropped"),
            ("?", "this card; any key goes back"),
        ]
    }
}

impl View for Watch<'_> {
    /// Commit whatever arrived, then compose the screen.
    ///
    /// The journal is read here rather than in [`View::tick`] because `frame`
    /// is the one method the loop calls every time round: a reader holding a
    /// key down would otherwise stop the turn arriving.
    fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        self.size = (cols, rows);
        self.live.resize(cols);
        self.settle();

        // Composed after the events above are settled, so the transcript the
        // card is hiding is still up to date the moment it comes down.
        if self.help {
            return render::keys::card(&self.theme, cols, rows, "turn", &self.map());
        }
        let block = self.live.block(self.started.elapsed());
        self.page.frame(cols, rows, &block, &self.hint(cols))
    }

    fn key(&mut self, key: Key) -> Flow {
        // The card is read, not worked in: the next key takes it down, and
        // that is all it does. Ctrl-C is answered before this, in `beat`, so
        // the card cannot hold up a turn the reader wants stopped.
        if self.help {
            self.help = false;
            return Flow::Go;
        }
        // A page is the body, less a row so the reader keeps their place in
        // the transcript, and never nothing on a small terminal.
        let page = self.size.1.saturating_sub(5).max(1) as isize;
        match key {
            Key::Char('q') | Key::Esc => return Flow::Stop,
            Key::Char('?') => self.help = true,
            Key::Up => self.page.scroll(1),
            Key::Down => self.page.scroll(-1),
            Key::PageUp => self.page.scroll(page),
            Key::PageDown => self.page.scroll(-page),
            Key::Home => self.page.scroll(isize::MAX),
            Key::End => self.page.follow(),
            _ => {}
        }
        Flow::Go
    }

    fn tick(&mut self) -> Flow {
        self.live.tick();
        Flow::Go
    }
}

impl Watch<'_> {
    /// Commit every journal event the page has not seen yet.
    ///
    /// The journal is polled rather than subscribed to, for the reason
    /// [`with_live`] gives, so this is the only way anything reaches the page.
    fn settle(&mut self) {
        let events = self.log.events();
        for event in events.iter().skip(self.seen) {
            let settled = self.live.push(&crossing(event));
            self.page.settle(settled);
        }
        self.seen = events.len();
    }

    /// The turn is over: `fault` is why, when it did not end well.
    ///
    /// Painted here rather than left to the next frame, because the frames are
    /// 80ms apart and this is the answer the reader has been waiting for.
    fn over<W: std::io::Write>(&mut self, out: &mut Ui<W>, fault: Option<&RpcError>) {
        // Settled first, so a failure lands after the last thing the turn
        // managed rather than in front of it.
        self.settle();
        if let Some(error) = fault {
            self.page
                .settle(render::fault::lines(&self.theme, self.size.0, error));
        }
        // Whether it failed or not: nothing more is coming, so the block that
        // says what the turn is waiting on has nothing left to say. Its going
        // is also what the footer reads back as `end` rather than `live`.
        self.live.finish();
        self.paint(out);
    }

    /// One turn of [`tetanus_ui::show`]'s loop, under this file's clock:
    /// answer every keystroke waiting, then paint.
    ///
    /// Ctrl-C and a resize are answered here and never reach [`View::key`],
    /// which is the contract `show` holds its views to. In raw mode Ctrl-C is
    /// a keystroke rather than a signal, so this is the only place it can be
    /// caught while the view is up.
    fn beat<W: std::io::Write>(&mut self, out: &mut Ui<W>, tty: &mut Tty<std::io::Stdout>) -> Flow {
        let mut flow = Flow::Go;
        // Every keystroke waiting, not one. A held arrow key repeats faster
        // than the frame interval, and a frame that answered one of them would
        // fall further behind the longer it was held. Bounded all the same, so
        // a pasted wall of text cannot hold up the turn running beside it.
        for _ in 0..KEYS {
            // No wait at all: the frame interval is this loop's clock, and a
            // read that blocked for one would cost the turn that time.
            let Some(key) = tty.key(std::time::Duration::ZERO).ok().flatten() else {
                // Nothing was typed, which is the wait `tick` answers.
                flow = self.tick();
                break;
            };
            flow = match key {
                Key::Ctrl('c') => Flow::Stop,
                Key::Resize(cols, rows) => {
                    self.size = (cols as usize, rows as usize);
                    Flow::Go
                }
                key => self.key(key),
            };
            if flow == Flow::Stop {
                break;
            }
        }
        self.paint(out);
        flow
    }

    /// Compose the screen at the size the terminal has now, and paint it if
    /// it is not already the screen on the terminal.
    ///
    /// This loop's clock is the turn's, not the reader's: it comes round every
    /// 80ms so that a spinner moves and a keystroke is answered without a wait
    /// anybody notices. The content does not change that often, and once the
    /// turn is over it does not change at all - `Live::block` is empty from
    /// there on, so every later frame is the previous one. The views driven by
    /// `tetanus_ui::show` never meet this, because a page over something
    /// finished waits an hour for a key; this one cannot wait, so it compares
    /// instead. Without the comparison a finished turn left on the screen goes
    /// on sending a repaint of it twelve times a second for as long as the
    /// reader reads it, which over a slow link is the difference between a
    /// still page and a flickering one.
    fn paint<W: std::io::Write>(&mut self, out: &mut Ui<W>) {
        let (cols, rows) = self.size;
        let frame = self.frame(cols, rows);
        if self.painted.as_ref() == Some(&frame) {
            return;
        }
        frame.paint(out).ok();
        self.painted = Some(frame);
    }
}

/// Stream the journal as contract output while the turn runs.
///
/// The same poll as [`with_live`], reporting to a script instead of a person:
/// every event lands on stdout as its own line as it arrives, and the caller
/// writes the result last. Nothing is drawn, so nothing has to be erased.
async fn with_json<W: std::io::Write, F: std::future::Future>(
    policy: &Policy,
    out: &mut Ui<W>,
    log: &JsonlSessionLog,
    phase: &str,
    work: F,
) -> Option<F::Output> {
    let mut status = status_line(policy, phase, false);
    let mut seen = 0;
    let mut frames = tokio::time::interval(std::time::Duration::from_millis(80));
    let mut work = std::pin::pin!(work);
    let mut stop = std::pin::pin!(interrupt());
    let done = loop {
        tokio::select! {
            done = &mut work => break Some(done),
            _ = &mut stop => break None,
            _ = frames.tick() => {
                seen = flush(out, log, seen);
                if let Some(status) = &mut status {
                    status.tick().ok();
                }
            }
        }
    };
    // Whatever the last poll did not catch. Every offline turn ends inside one
    // frame interval, so for those this is the whole stream.
    flush(out, log, seen);
    if let Some(status) = status {
        status.finish().ok();
    }
    done
}

/// Write every event the log gained since the last look, and report how many
/// it now holds.
fn flush<W: std::io::Write>(out: &mut Ui<W>, log: &JsonlSessionLog, seen: usize) -> usize {
    let events = log.events();
    for event in events.iter().skip(seen) {
        render::json::line(out, &crossing(event)).ok();
    }
    events.len()
}

/// Commit every event written since the last look, and report how many events
/// the log now holds.
fn settle<W: std::io::Write>(
    view: &mut Live,
    screen: &mut Screen<W>,
    log: &JsonlSessionLog,
    seen: usize,
) -> usize {
    let events = log.events();
    for event in events.iter().skip(seen) {
        let lines = view.push(&crossing(event));
        screen.print(&lines).ok();
    }
    events.len()
}

async fn run<W: std::io::Write>(
    policy: &Policy,
    document: &std::path::Path,
    out: &mut Ui<W>,
    args: RunArgs,
) -> Result<(), Reported> {
    // Before anything is opened. A view needs a screen to draw on, and a run
    // that cannot have one should say so at the point the flag was read rather
    // than after it has written a journal nobody will get to see.
    if args.ui && !policy.stdout_is_terminal {
        return Err(fail(policy, &nowhere_to_draw()));
    }

    // Then, before the journal exists: a prompt this build will not send is
    // a mistake to report at the point it was made, not one to record.
    let asked = prompt::resolve(args.ask.or(args.prompt), std::io::stdin().lock())
        .map_err(|err| fail(policy, &err))?;

    // What the turn runs on: the settings document, with the flags over it.
    // Before the journal is opened, because the document decides where the
    // journal goes when `--session` did not.
    let settled = turn_settings(
        policy,
        document,
        TurnFlags {
            adapter: args.adapter,
            model: args.model,
            max_steps: args.max_steps,
            session: args.session,
        },
        // A first run must need no credential, so an unconfigured `run` is
        // the mock adapter.
        AdapterChoice::Mock,
        "turn.jsonl",
    )?;

    let (adapter, model) = adapter(policy, settled.provider, settled.model.clone())?;

    // The journal is the engine's to open. `session.create` writes the
    // `session/start` header that makes the file self-describing - the model
    // it ran under, and the id every other call takes - which is what lets a
    // reader open a journal nobody told them about. The turn below still runs
    // in this process; it moves behind `agent.prompt` in its own slice, and
    // nothing here changes when it does.
    let opened = session(
        settled.settings.clone(),
        &settled.journal,
        settled.provider.route(),
        &model,
        settled.max_steps,
    )
    .await
    .map_err(|err| fail(policy, &about(err, &settled.journal)))?;

    let bus = EventBus::new();
    let log = JsonlSessionLog::create(&opened.session_id, &settled.journal, bus.clone())
        .map_err(|err| fail(policy, &journal_fault(&err, &settled.journal)))?;

    // Read the sequence with the same tracer the conformance suite uses.
    let trace = TurnTrace::attach(&bus);

    let ctx = boot(bus, adapter, Arc::new(registry()), log.clone())
        .map_err(|err| report(policy, &err.to_string(), None))?;
    let engine = TurnEngine::from_context(
        &ctx,
        TurnConfig {
            model: model.clone(),
            max_steps: settled.max_steps,
            ..TurnConfig::default()
        },
    )
    .map_err(|err| report(policy, &err.to_string(), None))?;

    // The name is drawn in the phase line and as the heading of the watched
    // view, and it arrived on a flag or out of a config file. The value that
    // chose the adapter above is the one that was given; this is the one that
    // is drawn.
    let named = tame_line(&model);
    let phase = format!("running the turn on {named}");
    let turn = engine.run_turn(&asked);
    let finished = match (args.trace, args.json, args.ui) {
        // The trace prints the sequence afterwards, so nothing may be written
        // above it while the turn runs.
        (true, _, _) => Some(with_progress(policy, &phase, turn).await),
        (_, true, _) => with_json(policy, out, &log, &phase, turn).await,
        (_, _, true) => {
            let watched = Watched {
                phase: &phase,
                think: args.think,
                title: &named,
            };
            with_page(policy, out, &log, watched, turn, |err| {
                turn_fault(
                    err,
                    &opened.session_id,
                    settled.provider.route(),
                    &settled.journal,
                )
            })
            .await
        }
        // From the first event: a run prints the whole of the journal it
        // opened - the header naming the model included, and for a resumed
        // journal the turns already on it.
        _ => with_live(policy, out, &log, 0, &phase, args.think, turn).await,
    };
    let Some(outcome) = finished else {
        // Stopped by the user. The journal is still worth naming: it holds
        // every event the turn managed before it was stopped. A script asked
        // for result types and gets none, because the call did not return one.
        if !args.json {
            journal(out, &log);
        }
        return Err(stopped(policy));
    };
    let outcome = outcome.map_err(|err| {
        fail(
            policy,
            &turn_fault(
                &err,
                &opened.session_id,
                settled.provider.route(),
                &settled.journal,
            ),
        )
    })?;
    engine
        .flush()
        .await
        .map_err(|err| report(policy, &err.to_string(), None))?;

    if args.trace {
        let theme: Theme = *out.theme();
        for (i, entry) in trace.entries().into_iter().enumerate() {
            // An in-memory extension point has no journal seq. Leave the
            // column blank rather than painting four spaces.
            let seq = match entry.seq {
                Some(seq) => format!("{:>4}", theme.paint(Role::Seq, &seq.to_string())),
                None => " ".repeat(4),
            };
            let mut line = format!("{i:>4}  {seq}  {}", theme.paint(Role::Topic, &entry.topic));
            if let Some(data) = entry.data.filter(|_| args.verbose) {
                line.push_str(&format!("  {data}"));
            }
            out.line(&line).ok();
        }
        out.blank().ok();
        out.field("turn", 7, &outcome.turn.to_string()).ok();
        out.field("steps", 7, &outcome.steps.to_string()).ok();
        out.field("stop", 7, outcome.reason.as_str()).ok();
        if let Some(veto) = &outcome.stop_veto {
            out.field("veto", 7, veto).ok();
        }
        // A debugging view still owes the user the answer they asked for.
        out.blank().ok();
        out.line(&outcome.content).ok();
    }

    if args.json {
        // The last line is the call's result, which is where a script stops
        // reading. Nothing else may follow it on stdout.
        let events: Vec<protocol::SessionEvent> = log.events().iter().map(crossing).collect();
        let result = AgentPromptResult {
            summary: render::json::summary(
                &events,
                outcome.turn,
                outcome.steps,
                reason(outcome.reason),
                outcome.stop_veto,
                outcome.content,
            ),
        };
        return render::json::line(out, &result)
            .map_err(|err| report(policy, &err.to_string(), None));
    }

    if args.ui {
        // The view had the whole screen and has given it back, and everything
        // drawn on it went with it. The answer is what the user asked for, so
        // it is written once more on the ordinary screen.
        out.line(&outcome.content).ok();
    }

    journal(out, &log);
    Ok(())
}

/// Open the journal this run writes to, through `session.create`.
///
/// The engine is built for this one call and dropped with it: it holds the
/// journal open while it writes the header, and the turn opens the same file
/// again to append to it. That is the seam this slice leaves - the call is the
/// contract's, the turn under it is not yet.
///
/// # Why the file name is offered as the id
///
/// A store resolves an id to `<root>/<id>.jsonl`, so a journal whose name is
/// not its id can be listed but not reopened by id. Offering the file name
/// keeps the id `tetanus sessions` prints an id the store answers to. A name
/// the store will not accept as an id is not an error - the run is about the
/// turn, not about the name - so the call is made again without it and the
/// store mints its own.
async fn session(
    settings: tetanus_engine::EngineConfig,
    path: &std::path::Path,
    provider: &str,
    model: &str,
    max_steps: u32,
) -> Result<protocol::SessionInfo, RpcError> {
    let engine = tetanus_engine::HarnessEngine::new(tetanus_engine::EngineConfig {
        // A named path is opened where it is, so the root is only what an
        // unnamed session would have fallen back to. Naming the journal's own
        // directory keeps the two answers the same.
        sessions_root: path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
        // Everything else is what the document settled: the retry policy a
        // turn runs under is in there, and an engine built from the compiled
        // defaults would quietly ignore it.
        ..settings
    });
    let named = tetanus_protocol::methods::SessionCreateParams {
        session_id: path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string),
        path: Some(path.display().to_string()),
        provider: Some(provider.to_string()),
        model: Some(model.to_string()),
        max_steps: Some(max_steps),
    };
    let anonymous = tetanus_protocol::methods::SessionCreateParams {
        session_id: None,
        ..named.clone()
    };
    match tetanus_protocol::methods::Engine::session_create(&engine, named).await {
        Err(rejected) if rejected.kind() == Some(ErrorCode::InvalidParams) => {
            tetanus_protocol::methods::Engine::session_create(&engine, anonymous).await
        }
        settled => settled,
    }
}

/// Where the durable record went. The last thing a run says, however it ended,
/// because it is the one thing the user cannot work out from the screen.
///
/// The path is tamed, not cut. It came off the command line, so it can carry
/// anything a shell can quote, and a row is a row wherever the value came
/// from. It keeps its full length because it is the one value on the page a
/// reader copies: a terminal folding a long path leaves it readable, and a cut
/// one sends them back to the flag they typed it on.
fn journal<W: std::io::Write>(out: &mut Ui<W>, log: &JsonlSessionLog) {
    out.blank().ok();
    let path = tame_line(&log.path().display().to_string());
    out.field("journal", 7, &path).ok();
}
