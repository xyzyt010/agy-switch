use clap::{Parser, Subcommand};

/// AGY-SWITCH — Antigravity Account Monitor & Switcher
#[derive(Parser, Debug)]
#[command(name = "agy-switch")]
#[command(about = "Antigravity account monitor and auto-switcher")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start the switcher daemon and launch the TUI
    On,
    /// Stop the switcher daemon and exit
    Off,
    /// Stop daemon, then start daemon + TUI
    Restart,
}
