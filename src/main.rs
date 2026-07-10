mod auth;
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
use std::io;

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
                show_start_prompt().await?;
            }
        }
    }

    Ok(())
}

async fn show_start_prompt() -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind},
        execute,
        style::Stylize,
        terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All))?;

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let center_y = rows / 2;

    execute!(stdout, crossterm::cursor::MoveTo(cols / 2 - 12, center_y - 4))?;
    execute!(stdout, crossterm::style::Print("AGY-SWITCH".bold().cyan()))?;

    execute!(stdout, crossterm::cursor::MoveTo(cols / 2 - 14, center_y - 1))?;
    execute!(stdout, crossterm::style::Print("Daemon is not running".dark_grey()))?;

    execute!(stdout, crossterm::cursor::MoveTo(cols / 2 - 22, center_y + 2))?;
    execute!(stdout, crossterm::style::Print(
        "Press ".white()
    ))?;
    execute!(stdout, crossterm::style::Print(
        "Enter".bold().green()
    ))?;
    execute!(stdout, crossterm::style::Print(
        " to start  or  ".white()
    ))?;
    execute!(stdout, crossterm::style::Print(
        "q".bold().red()
    ))?;
    execute!(stdout, crossterm::style::Print(
        " to quit".white()
    ))?;

    execute!(stdout, crossterm::cursor::MoveTo(cols / 2 - 28, center_y + 5))?;
    execute!(stdout, crossterm::style::Print(
        "You can also run: agy-switch on".dark_grey()
    ))?;

    loop {
        if let Event::Key(k) = event::read()? {
            if k.kind == KeyEventKind::Press {
                match k.code {
                    KeyCode::Enter => {
                        disable_raw_mode()?;
                        commands::on_off::turn_on().await?;
                        return Ok(());
                    }
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c')
                        if k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    Ok(())
}
