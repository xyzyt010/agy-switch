mod auth;
mod clipboard;
mod cli;
mod commands;
mod config;
mod daemon;
mod error;
mod http;
mod quota;
mod store;

use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Check for hidden __daemon argument
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "__daemon" {
        daemon::run_daemon().await?;
        return Ok(());
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::On) => {
            commands::on_off::turn_on().await?;
        }
        Some(Commands::Off) => {
            commands::on_off::turn_off().await?;
        }
        Some(Commands::Restart) => {
            commands::on_off::turn_off().await?;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            commands::on_off::turn_on().await?;
        }
        None => {
            if commands::on_off::is_daemon_running().await {
                commands::dashboard::run_dashboard().await?;
            } else {
                commands::on_off::turn_on().await?;
            }
        }
    }

    Ok(())
}
