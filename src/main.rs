use clap::{Parser, Subcommand};

mod clipboard;
mod proxy;
mod terminal;

#[derive(Parser)]
#[command(
    name = "xtrans",
    about = "SSH wrapper with clipboard image forwarding for remote Claude Code"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// SSH into remote with automatic clipboard image forwarding.
    /// Press Ctrl+V to paste clipboard content (images are uploaded automatically).
    Ssh {
        /// Arguments passed directly to ssh (e.g. user@host -p 2222)
        #[arg(trailing_var_arg = true, required = true)]
        ssh_args: Vec<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ssh { ssh_args } => proxy::run(&ssh_args),
    }
}
