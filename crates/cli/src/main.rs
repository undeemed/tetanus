//! The `tetanus` binary: run one documented turn headlessly.

mod render;

use tetanus_protocol::methods::{AgentPromptResult, ModelCatalogResult, SessionEventsResult};
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
    /// List model providers, the models they advertise, and what is reachable
    Models {
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long)]
        json: bool,
    },
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
        /// Print the model's thinking in full, not folded to its first line
        #[arg(long)]
        think: bool,
        /// Print the call's result as JSON: one object, per contract §4.7
        #[arg(long, conflicts_with_all = ["raw", "live"])]
        json: bool,
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
    /// Print the model's thinking in full, not folded to its first line
    #[arg(long)]
    think: bool,
    /// Print the events and the summary as JSON, per contract §4.7
    #[arg(long, conflicts_with = "trace")]
    json: bool,
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
        Cmd::Models { json } => {
            let catalog = providers();
            if json {
                return render::json::line(&mut out, &catalog)
                    .map_err(|err| report(policy, &err.to_string(), None));
            }
            render::catalog::models(&mut out, &catalog).ok();
            Ok(())
        }
        Cmd::Replay {
            path,
            raw,
            live,
            speed,
            think,
            json,
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

/// Carry a stop reason across. The engine's enum
/// is a subset of the contract's, which also names the reasons only a served
/// call can produce.
fn reason(reason: tetanus_turn::StopReason) -> protocol::StopReason {
    match reason {
        tetanus_turn::StopReason::Natural => protocol::StopReason::Natural,
        tetanus_turn::StopReason::PreStepRejected => protocol::StopReason::PreStepRejected,
        tetanus_turn::StopReason::MaxSteps => protocol::StopReason::MaxSteps,
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

/// Carry resolved config across to the contract shape the view reads.
///
/// The same story as [`boundary`]: the
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
    policy.stderr().warn("interrupted").ok();
    Reported(130)
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
///
/// Returns `None` when the user stopped the turn with Ctrl-C. The turn is
/// dropped where it stands; the block still comes off the screen, and the
/// journal keeps every event that had already been written to it.
async fn with_live<W: std::io::Write, F: std::future::Future>(
    policy: &Policy,
    out: &mut Ui<W>,
    log: &JsonlSessionLog,
    phase: &str,
    think: bool,
    work: F,
) -> Option<F::Output> {
    let (theme, width) = (*out.theme(), out.width());
    let mut view = Live::new(theme, width, phase, think);
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
    let mut status = match policy.stderr_is_terminal {
        false => None,
        true => {
            let mut status = policy.stderr_progress();
            status.set(phase).ok();
            Some(status)
        }
    };
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
    out: &mut Ui<W>,
    args: RunArgs,
) -> Result<(), Reported> {
    let (adapter, catalog): (Arc<dyn LlmAdapter>, Vec<String>) = match args.adapter {
        AdapterChoice::Mock => (Arc::new(mock::MockAdapter::new()), advertised(args.adapter)),
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
            (
                Arc::new(deepseek::DeepSeekAdapter::with_http(config)),
                advertised(args.adapter),
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
    let finished = match (args.trace, args.json) {
        // The trace prints the sequence afterwards, so nothing may be written
        // above it while the turn runs.
        (true, _) => Some(with_progress(policy, &phase, turn).await),
        (_, true) => with_json(policy, out, &log, &phase, turn).await,
        _ => with_live(policy, out, &log, &phase, args.think, turn).await,
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
    let outcome = outcome.map_err(|err| report(policy, &err.to_string(), None))?;
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

    journal(out, &log);
    Ok(())
}

/// Where the durable record went. The last thing a run says, however it ended,
/// because it is the one thing the user cannot work out from the screen.
fn journal<W: std::io::Write>(out: &mut Ui<W>, log: &JsonlSessionLog) {
    out.blank().ok();
    out.field("journal", 7, &log.path().display().to_string())
        .ok();
}
