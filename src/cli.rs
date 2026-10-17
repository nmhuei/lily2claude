use clap::{Parser, Subcommand};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "l2c",
    author = "light",
    version = "0.1.0",
    about = "Universal high-performance bridge connecting Claude Code CLI to Local Agents & OpenCode Free Tier"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Quick launch model override (when not using subcommand)
    #[arg(short, long, default_value = "deepseek")]
    pub model: String,

    /// Quick launch port override
    #[arg(long, default_value = "4001")]
    pub port: u16,

    /// Extra arguments passed directly to Claude Code
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra_args: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Start the proxy server daemon
    Serve {
        #[arg(short, long, default_value = "4001")]
        port: u16,
        #[arg(short = 'H', long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Launch Claude Code connected to lily2claude with auto-configured context & settings
    Launch {
        #[arg(short, long, default_value = "deepseek")]
        model: String,
        #[arg(short, long, default_value = "4001")]
        port: u16,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },
    /// List available models and aliases
    List {
        #[arg(short, long)]
        free_only: bool,
    },
    /// Healthcheck Docker WARP nodes and local agents
    Doctor,
}
