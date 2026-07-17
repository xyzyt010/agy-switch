use std::io;
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

struct TerminalGuard {
    active: std::cell::Cell<bool>,
}

impl TerminalGuard {
    fn new() -> Self {
        Self {
            active: std::cell::Cell::new(true),
        }
    }

    fn disarm(&self) {
        self.active.set(false);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active.get() {
            let _ = disable_raw_mode();
            let mut stdout = io::stdout();
            let _ = execute!(stdout, LeaveAlternateScreen);
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    MainMenu,
    SwitchAccounts,
    AccountsMenu,
    ContextMenu,
    ModeMenu,
    Help,
    TextInput,
    /// Shows JSON extracted from clipboard; Enter confirms import, Esc cancels
    PasteConfirm,
}

use crate::commands::account_add::ImportResult;

enum TextInputPurpose {
    ImportJson,
    ExportJson,
}

struct App {
    screen: Screen,
    menu_idx: usize,
    list_idx: usize,
    quit: bool,
    msg: String,
    msg_color: Color,

    /// Transient toast shown top-right (e.g. "Copied to clipboard")
    toast: Option<String>,
    toast_until: Option<std::time::Instant>,

    active_account_id: Option<Uuid>,
    mode: SwitchMode,

    list_scroll_offset: usize,

    last_quota_refresh: Option<chrono::DateTime<chrono::Utc>>,

    text_input: String,
    text_input_purpose: Option<TextInputPurpose>,

    /// Scroll offset for paste-confirm screen
    paste_scroll: usize,
    /// Stored JSON text for paste-confirm review (set before entering that screen)
    paste_json_buffer: String,
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
            toast: None,
            toast_until: None,
            active_account_id: state.active_account_id,
            mode: state.mode.clone(),
            list_scroll_offset: 0,
            last_quota_refresh: None,
            text_input: String::new(),
            text_input_purpose: None,
            paste_scroll: 0,
            paste_json_buffer: String::new(),
        }
    }

    fn set_msg(&mut self, msg: impl Into<String>, color: Color) {
        self.msg = msg.into();
        self.msg_color = color;
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        let s: String = msg.into();
        let pad = 24usize.saturating_sub(s.len());
        let padded = format!("{}{}", s, " ".repeat(pad));
        self.toast = Some(padded);
        self.toast_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
    }

    fn clear_toast_if_expired(&mut self) {
        if let Some(when) = self.toast_until {
            if std::time::Instant::now() > when {
                self.toast = None;
                self.toast_until = None;
            }
        }
    }
}

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
    "Paste accounts JSON (Clipboard)",
    "Copy accounts JSON (Clipboard)",
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

/// Hard limit on number of accounts. Used to refuse new logins/imports beyond.
const MAX_ACCOUNTS: usize = 150;

pub async fn run_dashboard() -> Result<(), AgySwitchError> {
    let mut store = FileStore::new(crate::config::accounts_path());
    store.load().await?;
    let mut state = load_state().await?;

    let _ = crate::store::active_writer::import_from_official_tools(&mut store).await;
    let _ = crate::store::active_writer::import_from_proxy_readonly(&mut store).await;

    enable_raw_mode().map_err(io_err)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(io_err)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(io_err)?;

    let guard = TerminalGuard::new();
    let mut app = App::new(&store, &state);
    let result = run_loop(&mut terminal, &mut store, &mut state, &mut app, &guard).await;
    guard.disarm();
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);
    result
}

fn io_err(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> AgySwitchError {
    AgySwitchError::Io(io::Error::new(io::ErrorKind::Other, e))
}

async fn run_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    store: &mut FileStore,
    state: &mut AppState,
    app: &mut App,
    guard: &TerminalGuard,
) -> Result<(), AgySwitchError> {
    let mut last_disk_reload = std::time::Instant::now();
    let disk_reload_interval = std::time::Duration::from_secs(3);
    let mut last_api_refresh = std::time::Instant::now();
    let api_refresh_interval = std::time::Duration::from_secs(20);

    loop {
        render(term, store, state, app)?;

        // In PasteConfirm input mode, buffer rapid keystrokes (terminal paste)
        // to avoid crashing the TUI with thousands of individual renders.
        // Only exits after 80ms of silence (paste burst is over).
        if app.screen == Screen::PasteConfirm && app.paste_json_buffer.is_empty() {
            let mut input_buf = String::new();
            let mut escape = false;
            // Keep reading events as long as they arrive within 80ms of each other
            loop {
                if crossterm::event::poll(std::time::Duration::from_millis(80)).map_err(io_err)? {
                    match crossterm::event::read().map_err(io_err)? {
                        Event::Key(k) if k.kind == KeyEventKind::Press => {
                            match k.code {
                                KeyCode::Esc => {
                                    escape = true;
                                    break;
                                }
                                KeyCode::Char(c) => {
                                    input_buf.push(c);
                                }
                                KeyCode::Enter => {
                                    input_buf.push('\n');
                                }
                                KeyCode::Backspace => {
                                    input_buf.pop();
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                } else {
                    break; // 80ms of silence — paste is done
                }
            }
            if escape {
                app.paste_json_buffer.clear();
                app.paste_scroll = 0;
                app.screen = Screen::AccountsMenu;
                app.menu_idx = 2;
                continue;
            }
            if !input_buf.is_empty() {
                app.paste_json_buffer = input_buf;
                app.paste_scroll = 0;
                continue; // Re-render with buffered paste
            }
            // No paste activity — fall through to normal 1s poll
        }

        let key = loop {
            if crossterm::event::poll(std::time::Duration::from_secs(1)).map_err(io_err)? {
                match crossterm::event::read().map_err(io_err)? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        break Some(k);
                    }
                    _ => {}
                }
            }

            if last_disk_reload.elapsed() >= disk_reload_interval {
                last_disk_reload = std::time::Instant::now();
                let _ = store.load().await;
                // Clamp list_idx to valid range after reload
                let count = store.count();
                if count == 0 {
                    app.list_idx = 0;
                } else if app.list_idx >= count {
                    app.list_idx = count - 1;
                }
                app.clear_toast_if_expired();
            }

            if last_api_refresh.elapsed() >= api_refresh_interval {
                last_api_refresh = std::time::Instant::now();
                match crate::store::active_writer::fetch_all_quotas(store).await {
                    Ok(n) if n > 0 => {
                        let _ = store.flush().await;
                        app.last_quota_refresh = Some(chrono::Utc::now());
                    }
                    Ok(_) => {
                        app.last_quota_refresh = Some(chrono::Utc::now());
                    }
                    Err(_) => {}
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
            Screen::AccountsMenu => handle_accounts_menu(key, app, store, state, guard, term).await?,
            Screen::ContextMenu => handle_context_menu(key, app, store, state).await?,
            Screen::ModeMenu => handle_mode_menu(key, app, state),
            Screen::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 4;
                }
            }
            Screen::TextInput => handle_text_input(key, app, store, guard).await?,
            Screen::PasteConfirm => handle_paste_confirm(key, app, store).await?,
        }

        if app.quit {
            break;
        }
    }
    Ok(())
}

/// Disarm the terminal: leave alt screen, disable raw mode.
/// Call `rearm_terminal` to resume. Best-effort.
fn disarm_terminal(guard: &TerminalGuard) {
    guard.disarm();
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);
}

/// Re-arm the terminal: re-enter alt screen, re-enable raw mode, clear cache.
fn rearm_terminal(
    guard: &TerminalGuard,
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), AgySwitchError> {
    enable_raw_mode().map_err(io_err)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(io_err)?;
    guard.active.set(true);
    // Invalidate the backend diff cache so the next draw repaints everything
    term.clear().map_err(io_err)?;
    Ok(())
}

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
                Constraint::Length(4),
                Constraint::Min(8),
                Constraint::Length(2),
            ])
            .split(area);

        draw_header(f, chunks[0], store, state);
        draw_content(f, chunks[1], store, state, app);
        draw_status_bar(f, chunks[2], app, store, state);

        // Toast overlay (top-right). Drawn last to overlay on top of header.
        if app.toast.is_some() {
            draw_toast(f, area, app);
        }
    })
    .map_err(io_err)?;
    Ok(())
}

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
        Span::styled(
            "                  \u{2190} Esc Go back",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let line2 = Line::from(vec![
        Span::raw("  "),
        Span::styled("Active: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            active_email.to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

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
        Screen::TextInput => draw_text_input(f, area, app),
        Screen::PasteConfirm => draw_paste_confirm(f, area, app),
    }
}

fn draw_text_input(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let title = match &app.text_input_purpose {
        Some(TextInputPurpose::ImportJson) => " Import from JSON ",
        Some(TextInputPurpose::ExportJson) => " Export to JSON ",
        None => " Input ",
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Enter file path (or press Esc to cancel):",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  > ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{}_", app.text_input),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Enter to confirm",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

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

    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let visible_height = (h[0].height as usize).saturating_sub(2);

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

            let (status_icon, status_color) = if !a.enabled {
                ("\u{2716}", Color::Red)
            } else if a.is_rate_limited {
                ("\u{26A0}", Color::Yellow)
            } else if is_active {
                ("\u{25CF}", Color::Green)
            } else {
                ("\u{25CB}", Color::DarkGray)
            };

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
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            area,
        );
    }
}

/// Top-right transient toast (e.g. "Copied to clipboard"). Auto-expires after 3s.
fn draw_toast(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some(msg) = &app.toast else { return };
    // Width: 32, Height: 3, top-right
    let w: u16 = 32;
    let h: u16 = 3;
    let x = area.width.saturating_sub(w).saturating_sub(1);
    let y: u16 = 1;
    let rect = Rect::new(x, y, w.min(area.width), h);
    let clear = ratatui::widgets::Clear;
    f.render_widget(clear, rect);
    let para = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(" \u{2714} {} ", msg.trim_end()),
            Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD),
        )),
    ]);
    f.render_widget(
        para,
        rect,
    );
}

/// Paste-confirm screen: input mode (empty buffer) or review mode (buffer filled).
fn draw_paste_confirm(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5), Constraint::Length(3)])
        .split(area);

    let is_input_mode = app.paste_json_buffer.is_empty();

    let header = Paragraph::new(Line::from(Span::styled(
        if is_input_mode {
            " Paste JSON below (Ctrl+Shift+V), then Enter "
        } else {
            "  \u{2191}\u{2193} Scroll   Enter Import   Esc Cancel "
        },
        Style::default().fg(Color::Yellow),
    )))
    .block(
        Block::default()
            .title(" Paste Accounts JSON ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(header, chunks[0]);

    if is_input_mode {
        let content = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Waiting for paste... (Ctrl+Shift+V or right-click)",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("  > ", Style::default().fg(Color::Green)),
                Span::styled(
                    "_",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
            ]),
        ])
        .block(
            Block::default()
                .title(Span::styled(
                    " Paste mode active ",
                    Style::default().fg(Color::Green),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(content, chunks[1]);
    } else {
        let lines: Vec<Line> = app
            .paste_json_buffer
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::White))))
            .collect();

        let visible_h = (chunks[1].height as usize).saturating_sub(2);
        let total = lines.len();
        let start = app.paste_scroll.min(total.saturating_sub(1));
        let end = (start + visible_h).min(total);
        let slice: Vec<Line> = lines.into_iter().skip(start).take(end - start).collect();

        let content = Paragraph::new(slice).block(
            Block::default()
                .title(Span::styled(
                    format!(" JSON (lines {}-{} of {}) ", start + 1, end, total),
                    Style::default().fg(Color::DarkGray),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(content, chunks[1]);
    }

    let status = if is_input_mode {
        Paragraph::new(Line::from(Span::styled(
            " Paste JSON   Enter Review   Esc Cancel ",
            Style::default().fg(Color::DarkGray),
        )))
    } else {
        let total = app.paste_json_buffer.lines().count();
        let start = app.paste_scroll.min(total.saturating_sub(1));
        Paragraph::new(Line::from(Span::styled(
            format!(
                " Lines: {}   \u{2191}\u{2193} Scroll   Enter Import   Esc Back ",
                total
            ),
            Style::default().fg(Color::DarkGray),
        )))
    };
    f.render_widget(status, chunks[2]);
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

    // Show remove hint at bottom of detail pane
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press r to remove this account",
        Style::default().fg(Color::Yellow),
    )));

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
        Line::from("    r                   Remove selected account"),
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
                " \u{2191}\u{2193} Navigate   Enter Switch   r Remove   Esc Back "
            } else {
                " \u{2191}\u{2193} Navigate   Enter Actions   r Remove   Esc Back "
            }
        }
        Screen::ContextMenu => " \u{2191}\u{2193} Navigate   Enter Select   Esc Back ",
        Screen::AccountsMenu => " \u{2191}\u{2193} Navigate   Enter Select   Esc Back ",
        Screen::ModeMenu => " \u{2191}\u{2193} Navigate   Enter Select   Esc Back ",
        Screen::Help => " Enter/Esc Back ",
        Screen::TextInput => " Type path   Enter Confirm   Esc Cancel ",
        Screen::PasteConfirm => " \u{2191}\u{2193} Scroll   Enter Import   Esc Cancel ",
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
                                app.set_msg(
                                    format!("Switch failed: {}", e),
                                    Color::Red,
                                );
                            }
                        }
                    }
                } else {
                    app.screen = Screen::ContextMenu;
                    app.menu_idx = 0;
                }
            }
        }
        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Char('r') => {
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
        KeyCode::Char('R') => {
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

async fn handle_accounts_menu(
    key: KeyEvent,
    app: &mut App,
    store: &mut FileStore,
    _state: &mut AppState,
    guard: &TerminalGuard,
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
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
                // Import from JSON file (path prompt)
                app.text_input.clear();
                app.text_input_purpose = Some(TextInputPurpose::ImportJson);
                app.screen = Screen::TextInput;
            }
            1 => {
                // Export to JSON file (path prompt)
                app.text_input.clear();
                app.text_input_purpose = Some(TextInputPurpose::ExportJson);
                app.screen = Screen::TextInput;
            }
            2 => {
                // Paste accounts JSON from clipboard → review in scrollable text box
                if store.count() >= MAX_ACCOUNTS {
                    app.set_msg(format!("Limit reached ({}). Remove accounts first.", MAX_ACCOUNTS), Color::Red);
                    return Ok(());
                }
                match crate::clipboard::get_text() {
                    Ok(text) if !text.trim().is_empty() => {
                        // Try to parse as JSON before showing — if invalid, show error
                        let parsed: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => {
                                app.set_msg(format!("Invalid JSON: {}", e), Color::Red);
                                return Ok(());
                            }
                        };
                        let accounts_arr = parsed
                            .get("accounts")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if accounts_arr.is_empty() {
                            app.set_msg("No accounts found in clipboard JSON", Color::Yellow);
                            return Ok(());
                        }
                        app.paste_json_buffer = text;
                        app.paste_scroll = 0;
                        app.set_toast(format!("Read {} bytes from clipboard", app.paste_json_buffer.len()));
                        app.screen = Screen::PasteConfirm;
                    }
                    _ => {
                        // Clipboard unavailable — enter input mode for terminal paste
                        app.paste_json_buffer.clear();
                        app.paste_scroll = 0;
                        app.screen = Screen::PasteConfirm;
                    }
                }
            }
            3 => {
                // Copy accounts JSON to clipboard
                let accounts = store.list();
                if accounts.is_empty() {
                    app.set_msg("No accounts to export", Color::Yellow);
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
                let json = match serde_json::to_string_pretty(&export) {
                    Ok(j) => j,
                    Err(e) => {
                        app.set_msg(format!("JSON error: {}", e), Color::Red);
                        return Ok(());
                    }
                };
                match crate::clipboard::set_text(&json) {
                    Ok(()) => {
                        app.set_toast(format!("Copied {} accounts to clipboard", accounts.len()));
                    }
                    Err(e) => {
                        app.set_msg(format!("Clipboard: {}", e), Color::Red);
                    }
                }
            }
            4 => {
                // Login via OAuth
                if store.count() >= MAX_ACCOUNTS {
                    app.set_msg(format!("Limit reached ({}). Cannot add more.", MAX_ACCOUNTS), Color::Red);
                    return Ok(());
                }

                disarm_terminal(guard);

                eprintln!("[AGY-SWITCH] Opening browser for OAuth login...");
                eprintln!("[AGY-SWITCH] Complete the login in your browser.");
                eprintln!("[AGY-SWITCH] Waiting for callback...");

                // Use Handle::current() to reuse the existing runtime (no nested runtime).
                // This is safe because disarm_terminal has already left the alt screen,
                // so the TUI is not driving crossterm::event::poll concurrently.
                let result = {
                    let mut s = FileStore::new(crate::config::accounts_path());
                    s.load().await?;
                    crate::commands::account_add::add_oauth_account(&mut s).await
                };

                store.load().await.unwrap_or(());

                rearm_terminal(guard, term)?;

                match result {
                    Ok(email) => {
                        app.set_msg(format!("Added: {}", email), Color::Green);
                    }
                    Err(e) => {
                        app.set_msg(format!("OAuth failed: {}", e), Color::Red);
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


/// Pins the accounts.json schema for the clipboard import/export format:
/// `{"version": 1, "accounts": [...]}`
async fn execute_clipboard_import(
    store: &mut FileStore,
    json_text: &str,
) -> Result<ImportResult, AgySwitchError> {
    // Count current accounts + incoming accounts; enforce 150-account hard limit
    let parsed: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| AgySwitchError::OAuthFailed(format!("JSON parse: {}", e)))?;
    let accounts_arr = parsed
        .get("accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut errors = Vec::new();
    let mut imported = 0u32;
    let mut updated = 0u32;
    let mut skipped = 0u32;

    for (i, acct) in accounts_arr.iter().enumerate() {
        // Enforce hard limit per-account (so we don't import past 150 total)
        if store.count() >= MAX_ACCOUNTS {
            errors.push(format!("Stopped at {} accounts (limit)", MAX_ACCOUNTS));
            skipped += (accounts_arr.len() - i) as u32;
            break;
        }

        let email = acct.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let access_token = acct.get("access_token").and_then(|v| v.as_str()).unwrap_or("");
        let refresh_token = acct.get("refresh_token").and_then(|v| v.as_str()).unwrap_or("");

        if email.is_empty() || refresh_token.is_empty() {
            errors.push(format!("Account {} missing email/refresh_token", i + 1));
            skipped += 1;
            continue;
        }

        let label = acct.get("label").and_then(|v| v.as_str()).map(String::from);
        let project_id = acct.get("project_id").and_then(|v| v.as_str()).map(String::from);
        let expiry_str = acct.get("expiry").and_then(|v| v.as_str()).unwrap_or("");
        let expiry = chrono::DateTime::parse_from_rfc3339(expiry_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now() + chrono::Duration::hours(1));

        // Update existing by email, else add new
        if let Some(existing) = store.get_by_email(email).cloned() {
            let mut updated_account = existing;
            updated_account.credential.access_token = access_token.to_string();
            updated_account.credential.refresh_token = refresh_token.to_string();
            updated_account.credential.expiry = expiry;
            updated_account.credential.project_id = project_id.or(updated_account.credential.project_id);
            if let Some(l) = label.clone() {
                updated_account.label = Some(l);
            }
            if let Err(e) = store.update(updated_account).await {
                errors.push(format!("{} update failed: {}", email, e));
            } else {
                updated += 1;
            }
        } else {
            let credential = crate::store::account::OAuthCredential {
                access_token: access_token.to_string(),
                refresh_token: refresh_token.to_string(),
                project_id,
                managed_project_id: None,
                expiry,
            };
            let account = crate::store::account::Account {
                id: uuid::Uuid::new_v4(),
                email: email.to_string(),
                label,
                credential,
                quota: None,
                added_at: chrono::Utc::now(),
                last_used_at: None,
                is_rate_limited: false,
                rate_limit_reset_at: None,
                enabled: true,
            };
            if let Err(e) = store.add(account).await {
                match e {
                    AgySwitchError::DuplicateAccount(_) => {
                        skipped += 1;
                    }
                    other => {
                        errors.push(format!("Add failed: {}", other));
                    }
                }
            } else {
                imported += 1;
            }
        }
    }

    store.flush().await?;
    Ok(ImportResult { imported, updated, skipped, errors })
}

/// Paste-confirm screen: when buffer empty = input mode (paste via terminal),
/// when buffer non-empty = review mode (Enter imports, scroll, Esc cancel).
async fn handle_paste_confirm(
    key: KeyEvent,
    app: &mut App,
    store: &mut FileStore,
) -> Result<(), AgySwitchError> {
    let line_count = app.paste_json_buffer.lines().count();
    match key.code {
        KeyCode::Esc => {
            app.paste_json_buffer.clear();
            app.paste_scroll = 0;
            app.screen = Screen::AccountsMenu;
            app.menu_idx = 2;
        }
        KeyCode::Enter if !app.paste_json_buffer.is_empty() => {
            let text = std::mem::take(&mut app.paste_json_buffer);
            app.set_msg("Importing...", Color::Yellow);
            match execute_clipboard_import(store, &text).await {
                Ok(r) => {
                    if r.errors.is_empty() {
                        app.set_msg(format!("Imported: {}", r.summary()), Color::Green);
                        app.set_toast(format!("Imported: {}", r.summary()));
                    } else {
                        app.set_msg(
                            format!("Imported: {} | Errors: {}", r.summary(), r.errors[0]),
                            Color::Yellow,
                        );
                    }
                }
                Err(e) => {
                    app.set_msg(format!("Import failed: {}", e), Color::Red);
                }
            }
            app.paste_scroll = 0;
            app.screen = Screen::MainMenu;
            app.menu_idx = 1;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.paste_scroll > 0 {
                app.paste_scroll -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.paste_scroll + 1 < line_count {
                app.paste_scroll += 1;
            }
        }
        KeyCode::PageUp => {
            app.paste_scroll = app.paste_scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            app.paste_scroll = (app.paste_scroll + 10).min(line_count.saturating_sub(1));
        }
        KeyCode::Home => {
            app.paste_scroll = 0;
        }
        KeyCode::End => {
            app.paste_scroll = line_count.saturating_sub(1);
        }
        _ => {}
    }
    Ok(())
}


async fn handle_text_input(
    key: KeyEvent,
    app: &mut App,
    store: &mut FileStore,
    _guard: &TerminalGuard,
) -> Result<(), AgySwitchError> {
    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::AccountsMenu;
            app.menu_idx = 0;
            app.text_input.clear();
            app.text_input_purpose = None;
        }
        KeyCode::Enter => {
            let path_str = app.text_input.trim().to_string();
            let purpose = app.text_input_purpose.take();

            if path_str.is_empty() {
                app.set_msg("No path entered", Color::Yellow);
                app.screen = Screen::AccountsMenu;
                app.menu_idx = 0;
                return Ok(());
            }

            match purpose {
                Some(TextInputPurpose::ImportJson) => {
                    let path = std::path::PathBuf::from(&path_str);
                    if !path.exists() {
                        app.set_msg(format!("File not found: {}", path_str), Color::Red);
                        app.screen = Screen::AccountsMenu;
                        app.menu_idx = 0;
                        return Ok(());
                    }

                    app.set_msg("Importing...".to_string(), Color::Yellow);

                    // Reuse the existing runtime (no nested runtime). We're on the TUI event loop,
                    // but we just `.await` directly — crossterm event::poll is not running concurrently.
                    let path_owned = path.clone();
                    let result: Result<ImportResult, AgySwitchError> = {
                        let mut s = FileStore::new(crate::config::accounts_path());
                        s.load().await?;
                        crate::commands::account_add::handle_add_json(&mut s, path_owned).await
                    };
                    let _ = path; // path moved into closure above

                    store.load().await.unwrap_or(());

                    match result {
                        Ok(r) => {
                            if r.errors.is_empty() {
                                app.set_msg(format!("Import: {}", r.summary()), Color::Green);
                                app.set_toast(format!("Import: {}", r.summary()));
                            } else {
                                app.set_msg(
                                    format!("Import: {} | Errors: {}", r.summary(), r.errors[0]),
                                    Color::Yellow,
                                );
                            }
                        }
                        Err(e) => {
                            app.set_msg(format!("Import failed: {}", e), Color::Red);
                        }
                    }

                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                }
                Some(TextInputPurpose::ExportJson) => {
                    let path = std::path::PathBuf::from(&path_str);
                    let accounts = store.list();

                    if accounts.is_empty() {
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
                            app.set_msg(
                                format!("Exported {} accounts to {}", accounts.len(), path.display()),
                                Color::Green,
                            );
                        }
                        Err(e) => {
                            app.set_msg(format!("Export failed: {}", e), Color::Red);
                        }
                    }

                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                }
                None => {
                    app.screen = Screen::MainMenu;
                    app.menu_idx = 1;
                }
            }
            app.text_input.clear();
        }
        KeyCode::Backspace => {
            app.text_input.pop();
        }
        KeyCode::Char(c) => {
            app.text_input.push(c);
        }
        _ => {}
    }
    Ok(())
}

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

fn compute_quota_remaining_str(a: &crate::store::account::Account) -> String {
    if !a.enabled {
        return "disabled".to_string();
    }
    if a.is_rate_limited {
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
        return "unknown".to_string();
    };
    if quota.models.is_empty() {
        return "unknown".to_string();
    }

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
        None => "unknown".to_string(),
    }
}

fn quota_bar(pct: u64) -> (String, Color) {
    let width = 10;
    let filled = ((pct as usize) * width / 100).min(width);
    let empty = width - filled;

    let mut s = String::with_capacity(width + 2);
    s.push(' ');
    for _ in 0..filled {
        s.push('\u{2588}');
    }
    for _ in 0..empty {
        s.push('\u{2591}');
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
