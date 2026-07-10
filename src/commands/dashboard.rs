use std::io::{self, Write};
use crossterm::{
    event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use crate::config::{AppState, SwitchMode, load_state};
use crate::error::AgySwitchError;
use crate::store::file_store::FileStore;

use uuid::Uuid;

/// RAII guard that restores the terminal on drop (even on panic).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

// ══════════════════════════════════════════════════════════════
//  SCREENS
// ══════════════════════════════════════════════════════════════

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    MainMenu,
    SwitchAccounts,
    AccountsMenu,
    ContextMenu,
    ModeMenu,
    Help,
}

struct App {
    screen: Screen,
    menu_idx: usize,
    list_idx: usize,
    quit: bool,
    msg: String,
    msg_color: Color,

    active_account_id: Option<Uuid>,
    mode: SwitchMode,

    /// Track the last render area for the switch list so we can scroll
    list_scroll_offset: usize,

    /// Last successful quota refresh time
    last_quota_refresh: Option<chrono::DateTime<chrono::Utc>>,
}

impl App {
    fn new(_store: &FileStore, state: &AppState) -> Self {
        App {
            screen: Screen::MainMenu,
            menu_idx: 0,
            list_idx: 0,
            quit: false,
            msg: String::new(),
            msg_color: Color::DarkGray,
            active_account_id: state.active_account_id,
            mode: state.mode.clone(),
            list_scroll_offset: 0,
            last_quota_refresh: None,
        }
    }

    fn set_msg(&mut self, msg: impl Into<String>, color: Color) {
        self.msg = msg.into();
        self.msg_color = color;
    }
}

// ══════════════════════════════════════════════════════════════
//  MENU ITEMS
// ══════════════════════════════════════════════════════════════

const MAIN_MENU: &[&str] = &[
    "Switch Accounts",
    "Accounts",
    "Switch Mode",
    "Help",
    "Quit",
];

const ACCOUNTS_MENU: &[&str] = &[
    "Import from JSON",
    "Export to JSON",
    "Login via OAuth",
];

const MODE_MENU: &[&str] = &[
    "AUTO  — switch when quota exhausted",
    "MANUAL — you control selection",
];

const CTX_MENU: &[&str] = &[
    "Activate this account",
    "Remove this account",
    "Back",
];


// ══════════════════════════════════════════════════════════════
//  PUBLIC ENTRY
// ══════════════════════════════════════════════════════════════

pub async fn run_dashboard() -> Result<(), AgySwitchError> {
    // Load store and state internally
    let mut store = FileStore::new(crate::config::accounts_path());
    store.load().await?;
    let mut state = load_state().await?;

    // Quick sync: only import from official tools (fast, no API calls)
    // Skip fetch_all_quotas at startup — let the daemon handle live quota updates
    let _ = crate::store::active_writer::import_from_official_tools(&mut store).await;
    let _ = crate::store::active_writer::import_from_proxy_readonly(&mut store).await;

    enable_raw_mode().map_err(io_err)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(io_err)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(io_err)?;

    let _guard = TerminalGuard;
    let mut app = App::new(&store, &state);
    run_loop(&mut terminal, &mut store, &mut state, &mut app).await
}

fn io_err(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> AgySwitchError {
    AgySwitchError::Io(io::Error::new(io::ErrorKind::Other, e))
}

// ══════════════════════════════════════════════════════════════
//  EVENT LOOP
// ══════════════════════════════════════════════════════════════

async fn run_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &mut FileStore,
    state: &mut AppState,
    app: &mut App,
) -> Result<(), AgySwitchError> {
    let mut last_disk_reload = std::time::Instant::now();
    let disk_reload_interval = std::time::Duration::from_secs(3);
    let mut last_api_refresh = std::time::Instant::now();
    let api_refresh_interval = std::time::Duration::from_secs(20);

    loop {
        render(term, store, state, app)?;

        // Poll for key events with a 1-second timeout so we can do periodic work
        let key = loop {
            if crossterm::event::poll(std::time::Duration::from_secs(1)).map_err(io_err)? {
                if let Event::Key(k) = crossterm::event::read().map_err(io_err)? {
                    if k.kind == KeyEventKind::Press {
                        break Some(k);
                    }
                }
            }

            // Periodic store reload from disk (daemon may have written new quota data)
            if last_disk_reload.elapsed() >= disk_reload_interval {
                last_disk_reload = std::time::Instant::now();
                let _ = store.load().await;
            }

            // Periodic API quota refresh (every 20s) — the TUI fetches its own quotas
            if last_api_refresh.elapsed() >= api_refresh_interval {
                last_api_refresh = std::time::Instant::now();
                match crate::store::active_writer::fetch_all_quotas(store).await {
                    Ok(n) if n > 0 => {
                        let _ = store.flush().await;
                        app.last_quota_refresh = Some(chrono::Utc::now());
                    }
                    Ok(_) => {
                        // No accounts to refresh, still update timestamp
                        app.last_quota_refresh = Some(chrono::Utc::now());
                    }
                    Err(_) => {
                        // Network error — keep last known data, don't clear quotas
                        // The store still has the previous quota values
                    }
                }
                render(term, store, state, app)?;
            }
        };

        let key = match key {
            Some(k) => k,
            None => continue,
        };

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            break;
        }

        match app.screen {
            Screen::MainMenu => handle_main_menu(key, app, store, state).await?,
            Screen::SwitchAccounts => handle_switch_accounts(key, app, store, state).await?,
            Screen::AccountsMenu => handle_accounts_menu(key, app, store, state).await?,
            Screen::ContextMenu => handle_context_menu(key, app, store, state).await?,
            Screen::ModeMenu => handle_mode_menu(key, app, state),
            Screen::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 4;
                }
            }
        }

        if app.quit {
            break;
        }
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════
//  RENDER
// ══════════════════════════════════════════════════════════════

fn render(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &FileStore,
    state: &AppState,
    app: &App,
) -> Result<(), AgySwitchError> {
    term.draw(|f| {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // Header
                Constraint::Min(8),    // Content
                Constraint::Length(2), // Status bar
            ])
            .split(area);

        draw_header(f, chunks[0], store, state);
        draw_content(f, chunks[1], store, state, app);
        draw_status_bar(f, chunks[2], app, store, state);
    })
    .map_err(io_err)?;
    Ok(())
}

// ── Header ──────────────────────────────────────────────────

fn draw_header(f: &mut ratatui::Frame, area: Rect, store: &FileStore, state: &AppState) {
    let accounts = store.list();
    let total = accounts.len();
    let active_count = accounts
        .iter()
        .filter(|a| state.active_account_id == Some(a.id))
        .count();

    let mode_text = match state.mode {
        SwitchMode::Auto => " AUTO ",
        SwitchMode::Manual => " MANUAL ",
    };
    let mode_color = match state.mode {
        SwitchMode::Auto => Color::Cyan,
        SwitchMode::Manual => Color::Yellow,
    };

    let (onoff, onoff_color) = if state.enabled {
        (" ON ", Color::Green)
    } else {
        (" OFF ", Color::Red)
    };

    let active_email = state
        .active_account_id
        .and_then(|id| store.get(id))
        .map(|a| a.email.as_str())
        .unwrap_or("none");

    // Line 1: Branding
    let line1 = Line::from(vec![
        Span::styled(
            " AGY-SWITCH ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            mode_text,
            Style::default()
                .fg(Color::Black)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            onoff,
            Style::default()
                .fg(Color::Black)
                .bg(onoff_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    // Line 2: Active account
    let line2 = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Active: ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            active_email.to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    // Line 3: Account stats
    let line3 = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{} total", total),
            Style::default().fg(Color::White),
        ),
        Span::styled("  |  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} active", active_count),
            Style::default().fg(Color::Green),
        ),
    ]);

    let header = Paragraph::new(vec![line1, line2, line3]).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " Dashboard ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(header, area);
}

// ── Content router ──────────────────────────────────────────

fn draw_content(
    f: &mut ratatui::Frame,
    area: Rect,
    store: &FileStore,
    state: &AppState,
    app: &App,
) {
    match app.screen {
        Screen::MainMenu => draw_menu(f, area, MAIN_MENU, app.menu_idx, " Main Menu "),
        Screen::SwitchAccounts => draw_switch_accounts(f, area, store, state, app),
        Screen::AccountsMenu => draw_menu(f, area, ACCOUNTS_MENU, app.menu_idx, " Accounts "),
        Screen::ContextMenu => draw_menu(f, area, CTX_MENU, app.menu_idx, " Account Actions "),
        Screen::ModeMenu => draw_mode_menu(f, area, state, app),
        Screen::Help => draw_help(f, area),
    }
}

// ── Generic menu renderer ───────────────────────────────────

fn draw_menu(f: &mut ratatui::Frame, area: Rect, items: &[&str], selected: usize, title: &str) {
    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, &label)| {
            let is_sel = i == selected;
            let prefix = if is_sel { " ► " } else { "   " };
            let (fg, bg, modifier) = if is_sel {
                (Color::Black, Color::Cyan, Modifier::BOLD)
            } else {
                (Color::White, Color::Rgb(20, 20, 30), Modifier::empty())
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(fg)
                        .bg(bg)
                        .add_modifier(modifier),
                ),
                Span::styled(
                    format!(" {} ", label),
                    Style::default()
                        .fg(fg)
                        .bg(bg)
                        .add_modifier(modifier),
                ),
            ]))
        })
        .collect();

    let mut ls = ListState::default();
    ls.select(Some(selected));

    f.render_stateful_widget(
        List::new(list_items)
            .block(
                Block::default()
                    .title(Span::styled(
                        title,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(Style::default())
            .highlight_symbol(""),
        area,
        &mut ls,
    );
}

// ── Switch Accounts screen ────────────────────────────────

fn draw_switch_accounts(
    f: &mut ratatui::Frame,
    area: Rect,
    store: &FileStore,
    state: &AppState,
    app: &App,
) {
    let accounts = store.list_sorted();

    if accounts.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No accounts loaded.",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Go to Main Menu → Accounts",
                Style::default().fg(Color::Cyan),
            )),
        ]);
        f.render_widget(
            empty.block(
                Block::default()
                    .title(Span::styled(
                        " Switch Accounts ",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            area,
        );
        return;
    }

    // Layout: left list (60%) + right detail (40%)
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // ── Left: account list ──
    let visible_height = (h[0].height as usize).saturating_sub(2); // subtract borders

    // Calculate scroll offset to keep selected item visible
    let scroll_offset = if app.list_idx >= app.list_scroll_offset + visible_height {
        app.list_idx - visible_height + 1
    } else if app.list_idx < app.list_scroll_offset {
        app.list_idx
    } else {
        app.list_scroll_offset
    };

    let items: Vec<ListItem> = accounts
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
        .map(|(i, a)| {
            let is_active = state.active_account_id == Some(a.id);
            let is_sel = i == app.list_idx;

            // Status icon
            let (status_icon, status_color) = if !a.enabled {
                ("\u{2716}", Color::Red) // ✖
            } else if a.is_rate_limited {
                ("\u{26A0}", Color::Yellow) // ⚠
            } else if is_active {
                ("\u{25CF}", Color::Green) // ●
            } else {
                ("\u{25CB}", Color::DarkGray) // ○
            };

            // Quota string
            let quota_str = compute_quota_remaining_str(a);

            let (fg, bg) = if is_sel {
                (Color::Black, Color::Cyan)
            } else if is_active {
                (Color::Green, Color::Rgb(15, 25, 15))
            } else {
                (Color::White, Color::Rgb(20, 20, 30))
            };

            let mut spans = vec![
                Span::styled(
                    if is_sel { " ► " } else { "   " },
                    Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", status_icon),
                    Style::default().fg(status_color).bg(bg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<28}", truncate(&a.email, 28)),
                    Style::default()
                        .fg(fg)
                        .bg(bg)
                        .add_modifier(if is_active || is_sel {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled(
                    format!("  {}", quota_str),
                    Style::default().fg(Color::DarkGray).bg(bg),
                ),
            ];

            if is_active {
                spans.push(Span::styled(
                    " ACTIVE",
                    Style::default()
                        .fg(Color::Green)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut ls = ListState::default();
    ls.select(Some(app.list_idx.saturating_sub(scroll_offset)));

    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Switch Accounts ({}) ", accounts.len()),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(Style::default())
            .highlight_symbol(""),
        h[0],
        &mut ls,
    );

    // ── Right: detail pane ──
    if let Some(a) = accounts.get(app.list_idx) {
        draw_detail_pane(f, h[1], a, state);
    } else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Select an account",
                Style::default().fg(Color::DarkGray),
            )))
            .block(
                Block::default()
                    .title(" Details ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
            h[1],
        );
    }
}

fn draw_detail_pane(
    f: &mut ratatui::Frame,
    area: Rect,
    a: &crate::store::account::Account,
    state: &AppState,
) {
    let is_active = state.active_account_id == Some(a.id);
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Email    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                a.email.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    if let Some(label) = &a.label {
        lines.push(Line::from(vec![
            Span::styled("  Name     ", Style::default().fg(Color::DarkGray)),
            Span::styled(label.clone(), Style::default().fg(Color::White)),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  Status   ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if a.is_rate_limited {
                "RATE LIMITED"
            } else if is_active {
                "ACTIVE"
            } else if a.enabled {
                "ENABLED"
            } else {
                "DISABLED"
            },
            Style::default().fg(if a.is_rate_limited {
                Color::Red
            } else if is_active {
                Color::Green
            } else if a.enabled {
                Color::White
            } else {
                Color::Red
            }),
        ),
    ]));

    if a.is_rate_limited {
        let mut rl_text = "RATE LIMITED".to_string();
        if let Some(reset) = a.rate_limit_reset_at {
            let now = chrono::Utc::now();
            if reset > now {
                let remaining = (reset - now).num_seconds();
                if remaining >= 3600 {
                    rl_text = format!("RATE LIMITED (resets in {}h)", remaining / 3600);
                } else if remaining >= 60 {
                    rl_text = format!("RATE LIMITED (resets in {}m)", remaining / 60);
                } else {
                    rl_text = format!("RATE LIMITED (resets in {}s)", remaining);
                }
            }
        }
        lines.push(Line::from(vec![
            Span::styled("  Warning  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                rl_text,
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Quota section
    if let Some(quota) = &a.quota {
        lines.push(Line::from(Span::styled(
            "  Quota",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

        if quota.models.is_empty() {
            lines.push(Line::from(Span::styled(
                "    No model data",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for m in &quota.models {
                let pct = m.remaining_fraction.map_or(100u64, |f| (f * 100.0).round() as u64);

                // If account is rate limited, ALL bars show as rate-limited (red)
                // regardless of the actual percentage — because the account can't use any model
                let (bar, bar_color, status_suffix) = if a.is_rate_limited {
                    let s = " ▓▓▓▓▓▓▓▓▓▓".to_string();
                    (s, Color::Red, " RATE LIMITED")
                } else if m.is_exhausted {
                    let s = " ░░░░░░░░░░".to_string();
                    (s, Color::DarkGray, " EXHAUSTED")
                } else {
                    let (b, c) = quota_bar(pct);
                    (b, c, "")
                };

                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        format!("{:<20}", truncate(&m.display_name, 20)),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {}%{}", pct, status_suffix),
                        Style::default().fg(if a.is_rate_limited || m.is_exhausted {
                            Color::Red
                        } else {
                            bar_color
                        }),
                    ),
                ]));
            }
        }
    } else {
        // No quota data at all
        let (status_text, status_color) = if a.is_rate_limited {
            ("  Rate limited (no quota data)", Color::Red)
        } else if !a.enabled {
            ("  Account disabled", Color::Red)
        } else {
            ("  No quota data available", Color::DarkGray)
        };
        lines.push(Line::from(Span::styled(
            status_text,
            Style::default().fg(status_color),
        )));
    }

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(Span::styled(
                        " Account Details ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
        area,
    );
}

// ── Mode menu (with current indicator) ──────────────────────

fn draw_mode_menu(f: &mut ratatui::Frame, area: Rect, state: &AppState, app: &App) {
    let current = match state.mode {
        SwitchMode::Auto => 0,
        SwitchMode::Manual => 1,
    };

    let items: Vec<ListItem> = MODE_MENU
        .iter()
        .enumerate()
        .map(|(i, &label)| {
            let is_sel = i == app.menu_idx;
            let is_cur = i == current;
            let prefix = if is_sel { " ► " } else { "   " };
            let marker = if is_cur { " ● " } else { "   " };

            let (fg, bg) = if is_sel {
                (Color::Black, Color::Cyan)
            } else {
                (Color::White, Color::Rgb(20, 20, 30))
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    marker,
                    Style::default()
                        .fg(Color::Green)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} ", label),
                    Style::default()
                        .fg(fg)
                        .bg(bg)
                        .add_modifier(if is_sel {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]))
        })
        .collect();

    let mut ls = ListState::default();
    ls.select(Some(app.menu_idx));

    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        " Switch Mode ",
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta)),
            )
            .highlight_style(Style::default())
            .highlight_symbol(""),
        area,
        &mut ls,
    );
}

// ── Help ────────────────────────────────────────────────────

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  CLI Commands",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    agy-switch             Show this TUI (if daemon running)"),
        Line::from("    agy-switch on          Start daemon + launch TUI"),
        Line::from("    agy-switch off         Stop daemon + exit"),
        Line::from("    agy-switch restart     Stop daemon, then start daemon + TUI"),
        Line::from(""),
        Line::from(Span::styled(
            "  Navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    \u{2191}\u{2193} / j/k         Move selection up/down"),
        Line::from("    Enter               Select / Activate account"),
        Line::from("    Esc                 Go back to previous screen"),
        Line::from("    R                   Refresh quota for all accounts"),
        Line::from("    x / Delete          Remove selected account"),
        Line::from("    Ctrl+C              Force quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  Main Menu",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    Switch Accounts     View & switch active account"),
        Line::from("    Accounts            Import JSON, Export JSON, Login OAuth"),
        Line::from("    Switch Mode         Auto or Manual switching"),
        Line::from("    Help                Show this help"),
        Line::from("    Quit                Exit the dashboard"),
        Line::from(""),
        Line::from(Span::styled(
            "  Accounts",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    Import from JSON    Import accounts from a JSON file"),
        Line::from("    Export to JSON      Export accounts to a JSON file"),
        Line::from("    Login via OAuth     Add account via Google browser login"),
        Line::from(""),
        Line::from(Span::styled(
            "  Switch Accounts",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    Manual mode:  Enter on account = switch to it"),
        Line::from("    Auto mode:    Enter on account = actions menu"),
        Line::from(""),
        Line::from(Span::styled(
            "  Account Sorting",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    1. Healthy (quota > 0%)  - A-Z"),
        Line::from("    2. Exhausted (0%)        - A-Z"),
        Line::from("    3. Rate limited          - A-Z"),
        Line::from("    4. Disabled              - A-Z"),
        Line::from(""),
        Line::from(Span::styled(
            "  Auto Mode",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("    Daemon checks quota every 10 seconds"),
        Line::from("    Instantly switches when active account exhausts"),
        Line::from("    Rate-limited accounts auto-switch too"),
        Line::from("    Skips exhausted & rate-limited when rotating"),
    ];

    f.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .title(Span::styled(
                    " Help ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

// ── Status bar (bottom) ─────────────────────────────────────

fn draw_status_bar(
    f: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    store: &FileStore,
    state: &AppState,
) {
    let hints = match app.screen {
        Screen::MainMenu => " \u{2191}\u{2193} Navigate   Enter Select   q Quit ",
        Screen::SwitchAccounts => {
            if app.mode == SwitchMode::Manual {
                " \u{2191}\u{2193} Navigate   Enter Switch   x Remove   Esc Back "
            } else {
                " \u{2191}\u{2193} Navigate   Enter Actions   x Remove   Esc Back "
            }
        }
        Screen::ContextMenu => " \u{2191}\u{2193} Navigate   Enter Select   Esc Back ",
        Screen::AccountsMenu => " \u{2191}\u{2193} Navigate   Enter Select   Esc Back ",
        Screen::ModeMenu => " \u{2191}\u{2193} Navigate   Enter Select   Esc Back ",
        Screen::Help => " Enter/Esc Back ",
    };

    let mem_kb = store.memory_usage() / 1024;
    let refresh_str = app.last_quota_refresh
        .map(|t| {
            let elapsed = (chrono::Utc::now() - t).num_seconds();
            if elapsed < 60 {
                format!("{}s ago", elapsed)
            } else if elapsed < 3600 {
                format!("{}m ago", elapsed / 60)
            } else {
                format!("{}h ago", elapsed / 3600)
            }
        })
        .unwrap_or_else(|| "never".to_string());
    let status_info = format!(
        " {} accounts | {} | {}KB | refreshed {} ",
        store.count(),
        if state.enabled { "Daemon ON" } else { "Daemon OFF" },
        mem_kb,
        refresh_str,
    );

    let line = if !app.msg.is_empty() {
        Line::from(Span::styled(
            format!(" {} ", app.msg),
            Style::default().fg(app.msg_color),
        ))
    } else {
        Line::from(vec![
            Span::styled(hints, Style::default().fg(Color::Cyan)),
            Span::styled(
                "                                              ",
                Style::default(),
            ),
            Span::styled(status_info, Style::default().fg(Color::DarkGray)),
        ])
    };

    f.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        area,
    );
}

// ══════════════════════════════════════════════════════════════
//  HANDLERS
// ══════════════════════════════════════════════════════════════

fn menu_up(idx: &mut usize, count: usize) {
    if count == 0 {
        return;
    }
    if *idx > 0 {
        *idx -= 1;
    } else {
        *idx = count - 1;
    }
}

fn menu_down(idx: &mut usize, count: usize) {
    if count == 0 {
        return;
    }
    if *idx < count - 1 {
        *idx += 1;
    } else {
        *idx = 0;
    }
}

// ── Main menu ───────────────────────────────────────────────

async fn handle_main_menu(
    key: KeyEvent,
    app: &mut App,
    _store: &mut FileStore,
    state: &mut AppState,
) -> Result<(), AgySwitchError> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => menu_up(&mut app.menu_idx, MAIN_MENU.len()),
        KeyCode::Down | KeyCode::Char('j') => menu_down(&mut app.menu_idx, MAIN_MENU.len()),
        KeyCode::Enter => match app.menu_idx {
            0 => {
                app.screen = Screen::SwitchAccounts;
                app.list_idx = 0;
                app.list_scroll_offset = 0;
            }
            1 => {
                app.screen = Screen::AccountsMenu;
                app.menu_idx = 0;
            }
            2 => {
                app.screen = Screen::ModeMenu;
                app.menu_idx = match state.mode {
                    SwitchMode::Auto => 0,
                    SwitchMode::Manual => 1,
                };
            }
            3 => {
                app.screen = Screen::Help;
            }
            4 => {
                app.quit = true;
            }
            _ => {}
        },
        KeyCode::Char('q') => {
            app.quit = true;
        }
        _ => {}
    }
    Ok(())
}

// ── Switch Accounts ─────────────────────────────────────────

async fn handle_switch_accounts(
    key: KeyEvent,
    app: &mut App,
    store: &mut FileStore,
    state: &mut AppState,
) -> Result<(), AgySwitchError> {
    let count = store.count();

    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::MainMenu;
            app.menu_idx = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.list_idx > 0 {
                app.list_idx -= 1;
            } else if count > 0 {
                app.list_idx = count - 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if count > 0 {
                app.list_idx = (app.list_idx + 1) % count;
            }
        }
        KeyCode::Enter => {
            if count > 0 && app.list_idx < count {
                if app.mode == SwitchMode::Manual {
                    // Manual mode: directly activate the selected account
                    if let Some(a) = store.list_sorted().get(app.list_idx).cloned() {
                        let account_clone = a.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            tokio::runtime::Handle::current().block_on(
                                crate::store::active_writer::write_active_account(&account_clone)
                            )
                        }).await
                        .map_err(|e| AgySwitchError::Io(io::Error::new(io::ErrorKind::Other, e)))?;

                        match result {
                            Ok(fresh_cred) => {
                                // Save the refreshed credential back to the store
                                if let Some(stored) = store.list_mut().iter_mut().find(|s| s.id == a.id) {
                                    stored.credential = fresh_cred;
                                }
                                state.active_account_id = Some(a.id);
                                app.active_account_id = Some(a.id);
                                // Persist state immediately so switching survives crashes
                                let _ = crate::config::save_state(state).await;
                                app.set_msg(
                                    format!("Switched to: {}", a.email),
                                    Color::Green,
                                );
                            }
                            Err(e) => {
                                app.set_msg(
                                    format!("Switch failed: {}", e),
                                    Color::Red,
                                );
                            }
                        }
                    }
                } else {
                    // Auto mode: open context menu
                    app.screen = Screen::ContextMenu;
                    app.menu_idx = 0;
                }
            }
        }
        // x or Delete: remove the selected account (works in both modes)
        KeyCode::Char('x') | KeyCode::Delete => {
            if count > 0 && app.list_idx < count {
                if let Some(a) = store.list_sorted().get(app.list_idx).cloned() {
                    let email = a.email.clone();
                    match crate::commands::account_remove::handle_remove(
                        store,
                        Some(email.clone()),
                        false,
                    )
                    .await
                    {
                        Ok(()) => {
                            app.set_msg(format!("Removed: {}", email), Color::Green);
                        }
                        Err(e) => {
                            app.set_msg(format!("Error: {}", e), Color::Red);
                        }
                    }
                    let new_count = store.count();
                    if new_count > 0 {
                        app.list_idx = app.list_idx.min(new_count - 1);
                    } else {
                        app.list_idx = 0;
                        state.active_account_id = None;
                    }
                }
            }
        }
        // R: refresh quota for all accounts
        KeyCode::Char('r') | KeyCode::Char('R') => {
            app.set_msg("Refreshing quota...".to_string(), Color::Yellow);
            match crate::store::active_writer::fetch_all_quotas(store).await {
                Ok(n) => {
                    let _ = store.flush().await;
                    app.set_msg(format!("Refreshed quota for {} accounts", n), Color::Green);
                }
                Err(e) => {
                    app.set_msg(format!("Refresh failed: {}", e), Color::Red);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ── Add menu ────────────────────────────────────────────────

async fn handle_accounts_menu(
    key: KeyEvent,
    app: &mut App,
    store: &mut FileStore,
    _state: &mut AppState,
) -> Result<(), AgySwitchError> {
    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::MainMenu;
            app.menu_idx = 1;
        }
        KeyCode::Up | KeyCode::Char('k') => menu_up(&mut app.menu_idx, ACCOUNTS_MENU.len()),
        KeyCode::Down | KeyCode::Char('j') => menu_down(&mut app.menu_idx, ACCOUNTS_MENU.len()),
        KeyCode::Enter => match app.menu_idx {
            0 => {
                // Import from JSON — use file path input instead of native dialog for SSH compatibility
                exit_tui();
                eprintln!("[AGY-SWITCH] Enter path to JSON file to import (or press Enter to cancel):");
                eprint!("> ");
                io::stdout().flush().unwrap_or(());
                let mut path_str = String::new();
                io::stdin().read_line(&mut path_str).unwrap_or(0);
                let path_str = path_str.trim().to_string();

                if path_str.is_empty() {
                    let _ = re_enter_tui();
                    app.set_msg("Import cancelled", Color::DarkGray);
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                    return Ok(());
                }

                let path = std::path::PathBuf::from(&path_str);
                if !path.exists() {
                    let _ = re_enter_tui();
                    app.set_msg(format!("File not found: {}", path_str), Color::Red);
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                    return Ok(());
                }

                eprintln!("[AGY-SWITCH] Importing accounts from: {}", path.display());

                let result = tokio::task::spawn_blocking(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            AgySwitchError::OAuthFailed(format!("Runtime: {}", e))
                        })?;
                    rt.block_on(async {
                        let mut s = FileStore::new(crate::config::accounts_path());
                        s.load().await?;
                        let r = crate::commands::account_add::handle_add_json(&mut s, path).await?;
                        Ok::<crate::commands::account_add::ImportResult, AgySwitchError>(r)
                    })
                })
                .await;

                // Always reload store and re-enter TUI, regardless of result
                store.load().await.unwrap_or(());
                let _ = re_enter_tui();

                match result {
                    Ok(Ok(r)) => {
                        if r.errors.is_empty() {
                            app.set_msg(format!("Import: {}", r.summary()), Color::Green);
                        } else {
                            app.set_msg(
                                format!("Import: {} | Errors: {}", r.summary(), r.errors[0]),
                                Color::Yellow,
                            );
                        }
                    }
                    Ok(Err(e)) => {
                        app.set_msg(format!("Import failed: {}", e), Color::Red);
                    }
                    Err(e) => {
                        app.set_msg(format!("Error: {}", e), Color::Red);
                    }
                }

                app.screen = Screen::MainMenu;
                app.menu_idx = 1;
            }
            1 => {
                // Export to JSON — use text prompt for SSH compatibility
                exit_tui();
                eprintln!("[AGY-SWITCH] Enter path to save JSON export (or press Enter to cancel):");
                eprint!("> ");
                io::stdout().flush().unwrap_or(());
                let mut path_str = String::new();
                io::stdin().read_line(&mut path_str).unwrap_or(0);
                let path_str = path_str.trim().to_string();

                if path_str.is_empty() {
                    let _ = re_enter_tui();
                    app.set_msg("Export cancelled", Color::DarkGray);
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                    return Ok(());
                }

                let path = std::path::PathBuf::from(&path_str);
                let accounts = store.list();

                if accounts.is_empty() {
                    let _ = re_enter_tui();
                    app.set_msg("No accounts to export", Color::Yellow);
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                    return Ok(());
                }

                let export_data: Vec<serde_json::Value> = accounts
                    .iter()
                    .map(|a| {
                        let mut obj = serde_json::json!({
                            "email": a.email,
                            "access_token": a.credential.access_token,
                            "refresh_token": a.credential.refresh_token,
                            "expiry": a.credential.expiry.to_rfc3339(),
                        });
                        if let Some(l) = &a.label {
                            obj["label"] = serde_json::json!(l);
                        }
                        if let Some(p) = &a.credential.project_id {
                            obj["project_id"] = serde_json::json!(p);
                        }
                        obj
                    })
                    .collect();
                let export = serde_json::json!({
                    "version": 1,
                    "accounts": export_data,
                });
                let json = serde_json::to_string_pretty(&export).map_err(AgySwitchError::Json)?;

                match std::fs::write(&path, &json) {
                    Ok(()) => {
                        let _ = re_enter_tui();
                        app.set_msg(
                            format!("Exported {} accounts to {}", accounts.len(), path.display()),
                            Color::Green,
                        );
                    }
                    Err(e) => {
                        let _ = re_enter_tui();
                        app.set_msg(format!("Export failed: {}", e), Color::Red);
                    }
                }

                app.screen = Screen::MainMenu;
                app.menu_idx = 1;
            }
            2 => {
                // Login via OAuth
                exit_tui();
                eprintln!("[AGY-SWITCH] Opening browser for OAuth login...");
                eprintln!("[AGY-SWITCH] Complete the login in your browser.");
                eprintln!("[AGY-SWITCH] Waiting for callback...");

                let result = tokio::task::spawn_blocking(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            AgySwitchError::OAuthFailed(format!("Runtime: {}", e))
                        })?;
                    rt.block_on(crate::commands::account_add::add_oauth_account(
                        &mut FileStore::new(crate::config::accounts_path()),
                    ))
                })
                .await;

                // Always reload store and re-enter TUI
                store.load().await.unwrap_or(());
                let _ = re_enter_tui();

                match result {
                    Ok(Ok(email)) => {
                        app.set_msg(format!("Added: {}", email), Color::Green);
                    }
                    Ok(Err(e)) => {
                        app.set_msg(format!("OAuth failed: {}", e), Color::Red);
                    }
                    Err(e) => {
                        app.set_msg(format!("Error: {}", e), Color::Red);
                    }
                }

                app.screen = Screen::MainMenu;
                app.menu_idx = 1;
            }
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

// ── Context menu (Auto mode only) ───────────────────────────

async fn handle_context_menu(
    key: KeyEvent,
    app: &mut App,
    store: &mut FileStore,
    state: &mut AppState,
) -> Result<(), AgySwitchError> {
    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::SwitchAccounts;
        }
        KeyCode::Up | KeyCode::Char('k') => menu_up(&mut app.menu_idx, CTX_MENU.len()),
        KeyCode::Down | KeyCode::Char('j') => menu_down(&mut app.menu_idx, CTX_MENU.len()),
        KeyCode::Enter => match app.menu_idx {
            0 => {
                // Activate
                if let Some(a) = store.list_sorted().get(app.list_idx).cloned() {
                    let account_clone = a.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        tokio::runtime::Handle::current().block_on(
                            crate::store::active_writer::write_active_account(&account_clone)
                        )
                    }).await
                    .map_err(|e| AgySwitchError::Io(io::Error::new(io::ErrorKind::Other, e)))?;

                    match result {
                        Ok(fresh_cred) => {
                            if let Some(stored) = store.list_mut().iter_mut().find(|s| s.id == a.id) {
                                stored.credential = fresh_cred;
                            }
                            state.active_account_id = Some(a.id);
                            app.active_account_id = Some(a.id);
                            let _ = crate::config::save_state(state).await;
                            app.set_msg(
                                format!("Switched to: {}", a.email),
                                Color::Green,
                            );
                        }
                        Err(e) => {
                            app.set_msg(format!("Switch failed: {}", e), Color::Red);
                        }
                    }
                }
                app.screen = Screen::SwitchAccounts;
            }
            1 => {
                // Remove
                if let Some(a) = store.list_sorted().get(app.list_idx).cloned() {
                    let email = a.email.clone();
                    match crate::commands::account_remove::handle_remove(
                        store,
                        Some(email.clone()),
                        false,
                    )
                    .await
                    {
                        Ok(()) => {
                            app.set_msg(format!("Removed: {}", email), Color::Green);
                        }
                        Err(e) => {
                            app.set_msg(format!("Error: {}", e), Color::Red);
                        }
                    }
                    let new_count = store.count();
                    if new_count > 0 {
                        app.list_idx = app.list_idx.min(new_count - 1);
                    } else {
                        app.list_idx = 0;
                        state.active_account_id = None;
                    }
                }
                app.screen = Screen::SwitchAccounts;
            }
            2 => {
                app.screen = Screen::SwitchAccounts;
            }
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

// ── Mode menu ───────────────────────────────────────────────

fn handle_mode_menu(key: KeyEvent, app: &mut App, state: &mut AppState) {
    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::MainMenu;
            app.menu_idx = 2;
        }
        KeyCode::Up | KeyCode::Char('k') => menu_up(&mut app.menu_idx, MODE_MENU.len()),
        KeyCode::Down | KeyCode::Char('j') => menu_down(&mut app.menu_idx, MODE_MENU.len()),
        KeyCode::Enter => {
            match app.menu_idx {
                0 => {
                    state.mode = SwitchMode::Auto;
                    app.mode = SwitchMode::Auto;
                    app.set_msg("Mode: AUTO", Color::Cyan);
                }
                1 => {
                    state.mode = SwitchMode::Manual;
                    app.mode = SwitchMode::Manual;
                    app.set_msg("Mode: MANUAL", Color::Yellow);
                }
                _ => {}
            }
            app.screen = Screen::MainMenu;
            app.menu_idx = 2;
        }
        _ => {}
    }
}

// ── Export menu ─────────────────────────────────────────────


// ══════════════════════════════════════════════════════════════
//  TUI EXIT / RE-ENTER
// ══════════════════════════════════════════════════════════════

fn exit_tui() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);
}

fn re_enter_tui() -> Result<(), AgySwitchError> {
    enable_raw_mode().map_err(io_err)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(io_err)?;
    Ok(())
}

// ══════════════════════════════════════════════════════════════
//  HELPERS
// ══════════════════════════════════════════════════════════════

/// Compute a human-readable quota remaining string for an account list row.
fn compute_quota_remaining_str(a: &crate::store::account::Account) -> String {
    if !a.enabled {
        return "disabled".to_string();
    }
    if a.is_rate_limited {
        // Show countdown if we have a reset time
        if let Some(reset) = a.rate_limit_reset_at {
            let now = chrono::Utc::now();
            if reset > now {
                let remaining = (reset - now).num_seconds();
                if remaining >= 3600 {
                    return format!("rate limited ({}h left)", remaining / 3600);
                } else if remaining >= 60 {
                    return format!("rate limited ({}m left)", remaining / 60);
                } else {
                    return format!("rate limited ({}s left)", remaining);
                }
            }
        }
        return "rate limited".to_string();
    }
    let Some(quota) = &a.quota else {
        return "healthy".to_string();
    };
    if quota.models.is_empty() {
        return "healthy".to_string();
    }

    // Find the model with the lowest remaining fraction
    let mut min_pct: Option<u64> = None;
    for m in &quota.models {
        if let Some(frac) = m.remaining_fraction {
            let pct = (frac * 100.0).round() as u64;
            min_pct = Some(min_pct.map_or(pct, |cur| cur.min(pct)));
        }
    }

    match min_pct {
        Some(pct) if pct <= 0 => "exhausted".to_string(),
        Some(pct) => format!("{}% left", pct),
        None => "healthy".to_string(),
    }
}

/// Render a colored progress bar for quota.
fn quota_bar(pct: u64) -> (String, Color) {
    let width = 10;
    let filled = ((pct as usize) * width / 100).min(width);
    let empty = width - filled;

    let mut s = String::with_capacity(width + 2);
    s.push(' ');
    for _ in 0..filled {
        s.push('\u{2588}'); // █
    }
    for _ in 0..empty {
        s.push('\u{2591}'); // ░
    }

    let color = if pct > 60 {
        Color::Green
    } else if pct > 30 {
        Color::Yellow
    } else if pct > 0 {
        Color::Red
    } else {
        Color::DarkGray
    };

    (s, color)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
