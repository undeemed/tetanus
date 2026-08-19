//! The `tetanus` binary: run one documented turn headlessly.

mod render;

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
use tetanus_ui::{ColorChoice, Policy, Role, Screen, Theme, Ui};

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
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one full turn and print the event sequence it emitted
    Run(RunArgs),
    /// Show resolved config with provenance
    Config,
    /// Replay a session journal
    Replay {
        /// Path to a JSONL journal a previous run wrote
        #[arg(value_name = "PATH")]
        path: String,
        /// Print one line per durable event instead of the timeline
        #[arg(long)]
        raw: bool,
        /// Play the turn back one event at a time, as it happened
        #[arg(long, conflicts_with = "raw")]
        live: bool,
        /// How much faster than the recorded pace to play. Default 1.
        #[arg(long, value_name = "N", requires = "live", value_parser = playback_speed)]
        speed: Option<f64>,
    },
    /// Print version/build info
    Info,
}

#[derive(clap::Args)]
struct RunArgs {
    /// What to ask the agent
    #[arg(short, long, value_name = "TEXT", default_value = "run one full turn")]
    prompt: String,
    /// Which model provider to resolve into the registry
    #[arg(short, long, value_enum, default_value_t = AdapterChoice::Mock)]
    adapter: AdapterChoice,
    /// Model id. Defaults to the adapter's first catalog entry.
    #[arg(short, long, value_name = "ID")]
    model: Option<String>,
    /// Where the session journal lands
    #[arg(
        short,
        long,
        value_name = "PATH",
        default_value = "sessions/turn.jsonl"
    )]
    session: PathBuf,
    /// Step budget for the turn
    #[arg(long, value_name = "N", default_value_t = 8)]
    max_steps: u32,
    /// Print the raw event sequence instead of the turn
    #[arg(long)]
    trace: bool,
    /// With `--trace`, print each event's payload next to its topic
    #[arg(long)]
    verbose: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AdapterChoice {
    /// Deterministic built-in adapter. No key, no network.
    Mock,
    /// DeepSeek chat completions. Needs `DEEPSEEK_API_KEY`.
    Deepseek,
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
            .styles(help::styles())
            .after_help(help::root_epilogue(&theme))
            .mut_subcommand("run", |run| run.after_help(help::run_epilogue(&theme)));
        <Self as clap::FromArgMatches>::from_arg_matches(&command.get_matches())
            .unwrap_or_else(|err| err.exit())
    }
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
    match cli.cmd {
        Cmd::Run(args) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| report(policy, &err.to_string(), None))?;
            runtime.block_on(run(policy, &mut out, args))
        }
        Cmd::Config => {
            let mut config = tetanus_config::Config::default();
            config.set("log.level", "info".into(), tetanus_config::Layer::Default);
            render::config::render(&mut out, &settings(&config)).ok();
            Ok(())
        }
        Cmd::Replay {
            path,
            raw,
            live,
            speed,
        } => {
            let events = tetanus_session::replay(&path)
                .map_err(|err| report(policy, &err.to_string(), None))?;
            if raw {
                let theme = *out.theme();
                for event in events {
                    let line = format!(
                        "{:>4}  {:<20} {}",
                        theme.paint(Role::Seq, &event.seq.to_string()),
                        theme.paint(Role::Topic, &event.ty),
                        event.data
                    );
                    out.line(&line).ok();
                }
                return Ok(());
            }
            let events = boundary(events);
            let animated = policy.stdout_is_terminal;
            match live {
                true => {
                    render::replay::play(&mut out, animated, &events, speed.unwrap_or(1.0)).ok()
                }
                false => render::timeline::render(&mut out, &events).ok(),
            };
            Ok(())
        }
        Cmd::Info => {
            let theme = *out.theme();
            let version = theme
                .paint(Role::Accent, env!("CARGO_PKG_VERSION"))
                .to_string();
            out.line(&format!("tetanus {version} - phase 1 core")).ok();
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

/// Carry resolved config across to the contract shape the view reads.
///
/// The second and last crossing, and the same story as [`boundary`]: the
/// layers agree one for one, so this is a copy. It goes when the engine serves
/// `config.dump`, and `render::config` does not notice.
fn settings(config: &tetanus_config::Config) -> Vec<protocol::ConfigEntry> {
    config
        .provenance()
        .map(|(key, resolved)| protocol::ConfigEntry {
            key: key.clone(),
            value: resolved.value.clone(),
            layer: match resolved.layer {
                tetanus_config::Layer::Default => protocol::ConfigLayer::Default,
                tetanus_config::Layer::File => protocol::ConfigLayer::File,
                tetanus_config::Layer::Env => protocol::ConfigLayer::Env,
                tetanus_config::Layer::Flag => protocol::ConfigLayer::Flag,
            },
        })
        .collect()
}

/// Write a diagnostic, with an optional next step, and pick an exit status.
fn report(policy: &Policy, message: &str, hint: Option<&str>) -> Reported {
    let mut err = policy.stderr();
    err.error(message).ok();
    if let Some(hint) = hint {
        err.note(hint).ok();
    }
    Reported(1)
}

/// Run `work` behind the status line, ticking the animation while it waits.
///
/// A live model call can hold for a long time with nothing to print, and a
/// surface that prints nothing looks hung. The line is on stderr and is erased
/// before anything else is written, so stdout is unchanged either way.
async fn with_progress<F: std::future::Future>(policy: &Policy, label: &str, work: F) -> F::Output {
    let mut progress = policy.stderr_progress();
    progress.set(label).ok();

    let mut work = std::pin::pin!(work);
    let mut frames = tokio::time::interval(std::time::Duration::from_millis(80));
    let done = loop {
        tokio::select! {
            done = &mut work => break done,
            _ = frames.tick() => { progress.tick().ok(); }
        }
    };
    progress.finish().ok();
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
/// The status line is for a piped run only. At a terminal the block's own
/// footer says what a status line would, and two spinners at once read as two
/// programs.
async fn with_live<W: std::io::Write, F: std::future::Future>(
    policy: &Policy,
    out: &mut Ui<W>,
    log: &JsonlSessionLog,
    phase: &str,
    work: F,
) -> F::Output {
    let (theme, width) = (*out.theme(), out.width());
    let mut view = Live::new(theme, width, phase);
    let mut screen = Screen::new(Ui::new(out.out(), theme, width), policy.stdout_is_terminal);
    let mut status = match policy.stdout_is_terminal {
        true => None,
        false => {
            let mut status = policy.stderr_progress();
            status.set(phase).ok();
            Some(status)
        }
    };

    let started = std::time::Instant::now();
    let mut seen = 0;
    let mut frames = tokio::time::interval(std::time::Duration::from_millis(80));
    let mut work = std::pin::pin!(work);
    let done = loop {
        tokio::select! {
            done = &mut work => break done,
            _ = frames.tick() => {
                seen = settle(&mut view, &mut screen, log, seen);
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
    out: &mut Ui<W>,
    args: RunArgs,
) -> Result<(), Reported> {
    let (adapter, catalog): (Arc<dyn LlmAdapter>, Vec<String>) = match args.adapter {
        AdapterChoice::Mock => (Arc::new(mock::MockAdapter::new()), vec![mock::MODEL.into()]),
        AdapterChoice::Deepseek => {
            let config = deepseek::DeepSeekConfig::default();
            if std::env::var(&config.api_key_env)
                .unwrap_or_default()
                .is_empty()
            {
                return Err(report(
                    policy,
                    &format!("{} is not set", config.api_key_env),
                    Some("run with `--adapter mock` for an offline turn"),
                ));
            }
            let catalog = config.models.clone();
            (
                Arc::new(deepseek::DeepSeekAdapter::with_http(config)),
                catalog,
            )
        }
    };
    let model = match args.model.or_else(|| catalog.first().cloned()) {
        Some(model) => model,
        None => {
            return Err(report(
                policy,
                "no model id and the adapter has an empty catalog",
                Some("name one with `--model <ID>`"),
            ))
        }
    };

    let bus = EventBus::new();
    let log = JsonlSessionLog::create("cli", &args.session, bus.clone())
        .map_err(|err| report(policy, &err.to_string(), None))?;

    // Read the sequence with the same tracer the conformance suite uses.
    let trace = TurnTrace::attach(&bus);

    let ctx = boot(
        bus,
        adapter,
        Arc::new(ToolRegistry::new().with(Arc::new(EchoTool))),
        log.clone(),
    )
    .map_err(|err| report(policy, &err.to_string(), None))?;
    let engine = TurnEngine::from_context(
        &ctx,
        TurnConfig {
            model: model.clone(),
            max_steps: args.max_steps,
            ..TurnConfig::default()
        },
    )
    .map_err(|err| report(policy, &err.to_string(), None))?;

    let phase = format!("running the turn on {model}");
    let turn = engine.run_turn(&args.prompt);
    let outcome = match args.trace {
        // The trace prints the sequence afterwards, so nothing may be written
        // above it while the turn runs.
        true => with_progress(policy, &phase, turn).await,
        false => with_live(policy, out, &log, &phase, turn).await,
    }
    .map_err(|err| report(policy, &err.to_string(), None))?;
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

    out.blank().ok();
    out.field("journal", 7, &log.path().display().to_string())
        .ok();
    Ok(())
}
