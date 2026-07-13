use anyhow::Result;
use clap::{Parser, Subcommand};

mod hook;
mod setup;

#[derive(Parser)]
#[command(name = "gw", about = "Coding agent panel for tmux")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the panel (default).
    Panel,
    /// Ingest one hook payload from stdin for the given provider.
    Hook { provider: String },
    /// Install hooks into provider configs (global).
    Setup {
        /// Remove previously installed hooks instead.
        #[arg(long)]
        remove: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd.unwrap_or(Cmd::Panel) {
        Cmd::Panel => panel(),
        Cmd::Hook { provider } => hook::run(&provider),
        Cmd::Setup { remove } => setup::run(remove),
    }
}

fn panel() -> Result<()> {
    anyhow::bail!("panel TUI not implemented yet")
}
