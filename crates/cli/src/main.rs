//! The `tetanus` binary: run one documented turn headlessly.

mod render;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use tetanus_core::EventBus;
use tetanus_session::JsonlSessionLog;
use tetanus_turn::boot::boot;
use tetanus_turn::llm::{deepseek, mock, LlmAdapter};
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnTrace};
use tetanus_ui::{ColorChoice, Policy, Role, Theme, Ui};

use render::help;

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
    /// Print the payload of every durable event next to its topic
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
            let theme = *out.theme();
            let entries: Vec<_> = config
                .provenance()
                .map(|(key, resolved)| (key.clone(), resolved.clone()))
                .collect();
            let pad = entries.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
            out.heading("config").ok();
            for (key, resolved) in &entries {
                let layer = format!("{:?}", resolved.layer).to_lowercase();
                // `serde_json::Value` ignores a format width, so pad the
                // string, not the value.
                let rendered = resolved.value.to_string();
                let value = format!("{rendered:<24}{}", theme.paint(Role::Muted, &layer));
                out.field(key, pad, &value).ok();
            }
            Ok(())
        }
        Cmd::Replay { path } => {
            let events = tetanus_session::replay(&path)
                .map_err(|err| report(policy, &err.to_string(), None))?;
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

    let outcome = with_progress(policy, &format!("running the turn on {model}"), async {
        engine.run_turn(&args.prompt).await
    })
    .await
    .map_err(|err| report(policy, &err.to_string(), None))?;
    engine
        .flush()
        .await
        .map_err(|err| report(policy, &err.to_string(), None))?;

    let theme: Theme = *out.theme();
    for (i, entry) in trace.entries().into_iter().enumerate() {
        // An in-memory extension point has no journal seq. Leave the column
        // blank rather than painting four spaces.
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

    let stop = outcome.reason.as_str().to_string();
    out.blank().ok();
    out.field("turn", 7, &outcome.turn.to_string()).ok();
    out.field("steps", 7, &outcome.steps.to_string()).ok();
    out.field("stop", 7, &stop).ok();
    if let Some(veto) = &outcome.stop_veto {
        out.field("veto", 7, veto).ok();
    }
    out.field("journal", 7, &log.path().display().to_string())
        .ok();
    out.blank().ok();
    out.line(&outcome.content).ok();
    Ok(())
}
