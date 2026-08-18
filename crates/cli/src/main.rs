use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tetanus", about = "tetanus — rust agent harness. everything deepseek-harness has, but better.")]
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
            let mut c = tetanus_config::Config::default();
            c.set("log.level", "info".into(), tetanus_config::Layer::Default);
            for (k, v) in c.provenance() {
                println!("{k} = {} ({:?})", v.value, v.layer);
            }
        }
        Cmd::Replay { path } => {
            let s = tetanus_session::Session::open(path);
            for ev in s.replay()? {
                println!("{} {}", ev.topic, ev.payload);
            }
        }
        Cmd::Info => println!("tetanus 0.1.0 — the rust you get from cutting yourself on the edge"),
    }
    Ok(())
}
