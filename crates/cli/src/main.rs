use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "harness", about = "Rust agent harness (name TBD by captain)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show resolved config with provenance
    Config,
    /// Replay a session journal
    Replay { path: String },
    /// Print version/build info
    Info,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Config => {
            let mut c = harness_config::Config::default();
            c.set("log.level", "info".into(), harness_config::Layer::Default);
            for (k, v) in c.provenance() {
                println!("{k} = {} ({:?})", v.value, v.layer);
            }
        }
        Cmd::Replay { path } => {
            let s = harness_session::Session::open(path);
            for ev in s.replay()? {
                println!("{} {}", ev.topic, ev.payload);
            }
        }
        Cmd::Info => println!("harness-rs 0.1.0 (pre-rename scaffold)"),
    }
    Ok(())
}
