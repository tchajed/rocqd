use anyhow::Result;
use rocqd::{client, daemon};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rocqd", about = "A caching daemon for Rocq compilation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon in foreground
    Start,
    /// Stop a running daemon
    Stop,
    /// Compile a Rocq file
    Compile {
        /// Path to the .v file
        file: String,
        /// Extra flags to pass to vsrocqtop
        #[arg(trailing_var_arg = true)]
        flags: Vec<String>,
    },
    /// Run a query against a compiled file
    Query {
        /// File and line in the format file.v:line
        file_line: String,
        /// Query text (e.g., "Check nat.")
        text: String,
    },
    /// Show daemon status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("rocqd=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Start => daemon::run().await,
        Command::Stop => client::stop().await,
        Command::Compile { file, flags } => client::compile(&file, &flags).await,
        Command::Query { file_line, text } => client::query(&file_line, &text).await,
        Command::Status => client::status().await,
    }
}
