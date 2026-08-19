//! The `tetanus` binary: run one documented turn headlessly.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use tetanus_core::EventBus;
use tetanus_session::JsonlSessionLog;
use tetanus_turn::boot::boot;
use tetanus_turn::llm::{deepseek, mock, LlmAdapter};
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnTrace};

#[derive(Parser)]
#[command(
    name = "tetanus",
    about = "tetanus - rust agent harness. everything deepseek-harness has, but better."
)]
struct Cli {
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
    Replay { path: String },
    /// Print version/build info
    Info,
}

#[derive(clap::Args)]
struct RunArgs {
    /// What to ask the agent
    #[arg(short, long, default_value = "run one full turn")]
    prompt: String,
    /// Which model provider to resolve into the registry
    #[arg(short, long, value_enum, default_value_t = AdapterChoice::Mock)]
    adapter: AdapterChoice,
    /// Model id. Defaults to the adapter's first catalog entry.
    #[arg(short, long)]
    model: Option<String>,
    /// Where the session journal lands
    #[arg(short, long, default_value = "sessions/turn.jsonl")]
    session: PathBuf,
    /// Step budget for the turn
    #[arg(long, default_value_t = 8)]
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

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Run(args) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(run(args)),
        Cmd::Config => {
            let mut c = tetanus_config::Config::default();
            c.set("log.level", "info".into(), tetanus_config::Layer::Default);
            for (k, v) in c.provenance() {
                println!("{k} = {} ({:?})", v.value, v.layer);
            }
            Ok(())
        }
        Cmd::Replay { path } => {
            for ev in tetanus_session::replay(&path)? {
                println!("{:>4}  {:<20} {}", ev.seq, ev.ty, ev.data);
            }
            Ok(())
        }
        Cmd::Info => {
            println!("tetanus {} - phase 1 core", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

async fn run(args: RunArgs) -> anyhow::Result<()> {
    let (adapter, catalog): (Arc<dyn LlmAdapter>, Vec<String>) = match args.adapter {
        AdapterChoice::Mock => (Arc::new(mock::MockAdapter::new()), vec![mock::MODEL.into()]),
        AdapterChoice::Deepseek => {
            let config = deepseek::DeepSeekConfig::default();
            if std::env::var(&config.api_key_env)
                .unwrap_or_default()
                .is_empty()
            {
                anyhow::bail!(
                    "{} is not set. Run with `--adapter mock` for an offline turn.",
                    config.api_key_env
                );
            }
            let catalog = config.models.clone();
            (
                Arc::new(deepseek::DeepSeekAdapter::with_http(config)),
                catalog,
            )
        }
    };
    let model = args
        .model
        .or_else(|| catalog.first().cloned())
        .ok_or_else(|| anyhow::anyhow!("no model id and the adapter has an empty catalog"))?;

    let bus = EventBus::new();
    let log = JsonlSessionLog::create("cli", &args.session, bus.clone())?;

    // Read the sequence with the same tracer the conformance suite uses.
    let trace = TurnTrace::attach(&bus);

    let ctx = boot(
        bus,
        adapter,
        Arc::new(ToolRegistry::new().with(Arc::new(EchoTool))),
        log.clone(),
    )?;
    let engine = TurnEngine::from_context(
        &ctx,
        TurnConfig {
            model,
            max_steps: args.max_steps,
            ..TurnConfig::default()
        },
    )?;

    let outcome = engine.run_turn(&args.prompt).await?;
    engine.flush().await?;

    for (i, entry) in trace.entries().into_iter().enumerate() {
        let seq = entry.seq.map(|s| s.to_string()).unwrap_or_default();
        print!("{i:>4}  {seq:>4}  {}", entry.topic);
        match entry.data.filter(|_| args.verbose) {
            Some(data) => println!("  {data}"),
            None => println!(),
        }
    }
    println!();
    println!("turn    {}", outcome.turn);
    println!("steps   {}", outcome.steps);
    println!("stop    {}", outcome.reason.as_str());
    if let Some(veto) = &outcome.stop_veto {
        println!("veto    {veto}");
    }
    println!("journal {}", log.path().display());
    println!();
    println!("{}", outcome.content);
    Ok(())
}
