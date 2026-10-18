use clap::{Parser, Subcommand};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "l2c",
    author = "light",
    version = "0.1.0",
    about = "Universal high-performance bridge connecting Claude Code CLI to Local Agents & OpenCode Free Tier",
    subcommand_precedence_over_arg = true
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
    #[command(alias = "daemon")]
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
    /// List available models and aliases
    Models {
        #[arg(short, long)]
        free_only: bool,
    },
    /// Healthcheck Docker WARP nodes and local agents
    #[command(alias = "diagnose", alias = "check")]
    Doctor,
    /// Display real-time token usage, cost ledger, and session statistics
    #[command(alias = "usage", alias = "tokens")]
    Stats {
        /// Target port of the running lily2claude daemon
        #[arg(short, long, default_value = "4001")]
        port: u16,
        /// Filter statistics for a specific session ID
        #[arg(short, long)]
        session: Option<String>,
        /// Output raw JSON instead of human-friendly terminal report
        #[arg(long)]
        json: bool,
    },
}

