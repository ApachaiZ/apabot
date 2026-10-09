//! Mission control : TUI interactif au clavier.
//!
//! Trois zones :
//! - l'EN-TÊTE : état vivant du daemon (pid, uptime, redémarrages, bot) ;
//! - les LOGS : les dernières lignes de `logs/apabot.log`, suivies en
//!   direct (défilement ↑/↓/PgUp/PgDn, suivi repris avec `f`) ;
//! - la BARRE DE COMMANDE : `:` ouvre la saisie — on y lance `start`,
//!   `stop`, `restart`, `status`, `help`, `quit` depuis l'outil lui-même.
//!
//! `q` quitte, `?` affiche l'aide. Le tout sans PM2, sans systemd : le TUI
//! pilote le superviseur embarqué (`ops::supervise`) via son canal de
//! contrôle et lit le fichier de log directement (voir `ops::tail`).

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::time::{Duration, Instant};

use crate::i18n::{fill, Catalog};
use crate::ops::{self, tail};

/// Nombre maximal de lignes conservées en mémoire (au-delà, les plus
/// anciennes sortent — le fichier, lui, garde tout).
const RING_CAPACITY: usize = 500;

struct Tui {
    lines: Vec<String>,
    /// Lignes masquées en bas (0 = collé à la fin).
    scroll: usize,
    follow: bool,
    help: bool,
    quit: bool,
    command_mode: bool,
    input: String,
    message: Option<String>,
    snapshot: ops::Snapshot,
    follower: Option<tail::Follower>,
    /// Action utilisateur en cours (start/stop/restart), rapportée ici.
    pending: Option<tokio::task::JoinHandle<Result<String, String>>>,
}

impl Tui {
    fn push_log(&mut self, line: String) {
        if self.lines.len() >= RING_CAPACITY {
            let excess = self.lines.len() - RING_CAPACITY + 1;
            self.lines.drain(0..excess);
        }
        self.lines.push(line);
    }
}

/// Point d'entrée du TUI. Restaure le terminal quoi qu'il arrive.
pub async fn run(lang: &'static str, catalog: &'static Catalog) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    crossterm::terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = mission_loop(&mut terminal, lang, catalog).await;

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn mission_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    lang: &'static str,
    catalog: &'static Catalog,
) -> Result<(), Box<dyn std::error::Error>> {
    // ── Lecteur d'événements clavier (thread dédié, canal bloquant) ──
    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    std::thread::spawn(move || loop {
        if event::poll(Duration::from_millis(100)).unwrap_or(false) {
            match event::read() {
                Ok(ev) => {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // ── État initial : les dernières lignes déjà écrites, pas de recopie ──
    let mut tui = Tui {
        lines: Vec::new(),
        scroll: 0,
        follow: true,
        help: false,
        quit: false,
        command_mode: false,
        input: String::new(),
        message: None,
        snapshot: ops::Snapshot::default(),
        follower: None,
        pending: None,
    };
    let log_path = crate::paths::log_file();
    let (seed, offset) = tail::tail_lines(&log_path, 50);
    for line in seed {
        tui.push_log(line);
    }
    tui.follower = Some(tail::Follower::new(offset));

    let mut status_task: Option<tokio::task::JoinHandle<ops::Snapshot>> = None;
    let mut last_status = Instant::now() - Duration::from_secs(2); // refresh immédiat
    let mut last_log = Instant::now();

    while !tui.quit {
        // ── Événements clavier ──
        for ev in rx.try_iter() {
            handle_event(&mut tui, ev, lang, catalog);
        }

        // ── Rafraîchissement de l'état du daemon (toutes les 2 s) ──
        if last_status.elapsed() >= Duration::from_secs(2) && status_task.is_none() {
            status_task = Some(tokio::spawn(ops::status_snapshot()));
            last_status = Instant::now();
        }
        if let Some(task) = &mut status_task {
            if task.is_finished() {
                if let Ok(snapshot) = task.await {
                    tui.snapshot = snapshot;
                }
                status_task = None;
            }
        }

        // ── Nouvelles lignes de log (toutes les 300 ms) ──
        if last_log.elapsed() >= tail::POLL_INTERVAL {
            if let Some(follower) = &mut tui.follower {
                for line in follower.poll(&log_path) {
                    tui.push_log(line);
                }
            }
            last_log = Instant::now();
        }

        // ── Résultat d'une action utilisateur ──
        if let Some(task) = &mut tui.pending {
            if task.is_finished() {
                let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
                tui.message = Some(match result {
                    Ok(msg) => msg,
                    Err(msg) => msg,
                });
                tui.pending = None;
                // L'état vient de changer : rafraîchir sans attendre le tick.
                last_status = Instant::now() - Duration::from_secs(2);
            }
        }

        terminal.draw(|f| render(f, &tui, catalog))?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

// ── Clavier ────────────────────────────────────────────────────────────────

fn handle_event(tui: &mut Tui, ev: Event, lang: &'static str, catalog: &'static Catalog) {
    let Event::Key(key) = ev else { return };
    if key.kind != KeyEventKind::Press {
        return;
    }

    if tui.command_mode {
        match key.code {
            KeyCode::Esc => {
                tui.command_mode = false;
                tui.input.clear();
            }
            KeyCode::Enter => {
                let cmd = tui.input.trim().to_string();
                tui.input.clear();
                tui.command_mode = false;
                execute_command(tui, &cmd, lang, catalog);
            }
            KeyCode::Backspace => {
                tui.input.pop();
            }
            KeyCode::Char(c) => tui.input.push(c),
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => tui.quit = true,
        KeyCode::Esc => {
            if tui.help {
                tui.help = false;
            }
        }
        KeyCode::Char('s') => spawn_action(tui, ops::cmd_start(lang, catalog)),
        KeyCode::Char('x') => spawn_action(tui, ops::cmd_stop(catalog)),
        KeyCode::Char('r') => spawn_action(tui, ops::cmd_restart(lang, catalog)),
        KeyCode::Char('f') => {
            tui.follow = !tui.follow;
            if tui.follow {
                tui.scroll = 0;
            }
        }
        KeyCode::Char('?') => tui.help = !tui.help,
        KeyCode::Char(':') => {
            tui.command_mode = true;
            tui.input.clear();
        }
        KeyCode::Up => {
            tui.scroll = (tui.scroll + 1).min(tui.lines.len());
            tui.follow = false;
        }
        KeyCode::Down => {
            tui.scroll = tui.scroll.saturating_sub(1);
            if tui.scroll == 0 {
                tui.follow = true;
            }
        }
        KeyCode::PageUp => {
            tui.scroll = (tui.scroll + 10).min(tui.lines.len());
            tui.follow = false;
        }
        KeyCode::PageDown => {
            tui.scroll = tui.scroll.saturating_sub(10);
            if tui.scroll == 0 {
                tui.follow = true;
            }
        }
        KeyCode::End => {
            tui.scroll = 0;
            tui.follow = true;
        }
        KeyCode::Home => {
            tui.scroll = tui.lines.len();
            tui.follow = false;
        }
        _ => {}
    }
}

/// Commandes tapées dans la barre `: …` — les mêmes verbes que la CLI,
/// exécutées DEPUIS l'outil.
fn execute_command(tui: &mut Tui, cmd: &str, lang: &'static str, catalog: &'static Catalog) {
    match cmd {
        "start" => spawn_action(tui, ops::cmd_start(lang, catalog)),
        "stop" => spawn_action(tui, ops::cmd_stop(catalog)),
        "restart" => spawn_action(tui, ops::cmd_restart(lang, catalog)),
        "status" => {
            tui.message = Some(status_line(tui, catalog));
        }
        "help" | "?" => tui.help = true,
        "quit" | "q" | "exit" => tui.quit = true,
        "" => {}
        other => {
            tui.message = Some(fill(catalog.ops.tui_unknown_cmd, &[("cmd", other)]));
        }
    }
}

fn spawn_action<F>(tui: &mut Tui, fut: F)
where
    F: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    tui.pending = Some(tokio::spawn(fut));
    tui.message = Some("…".to_string());
}

// ── Rendu ──────────────────────────────────────────────────────────────────

fn status_line(tui: &Tui, catalog: &Catalog) -> String {
    let s = &tui.snapshot;
    if s.running {
        let bot = s
            .child_pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string());
        format!(
            "{} — {} {}, {} {}, {} {}, {} {}",
            catalog.ops.tui_running,
            catalog.ops.tui_pid,
            s.pid.unwrap_or(0),
            catalog.ops.tui_uptime,
            ops::human_duration(s.uptime_secs),
            catalog.ops.tui_restarts,
            s.restarts,
            catalog.ops.tui_bot,
            bot,
        )
    } else {
        catalog.ops.tui_no_daemon.to_string()
    }
}

fn render(f: &mut Frame, tui: &Tui, catalog: &Catalog) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // en-tête
            Constraint::Length(1), // message
            Constraint::Min(4),    // logs / aide
            Constraint::Length(1), // barre de commande
            Constraint::Length(1), // aide clavier
        ])
        .split(f.area());

    // ── En-tête ──
    let header = Paragraph::new(status_line(tui, catalog))
        .style(Style::default().fg(Color::Cyan).add_modifier(ratatui::style::Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    // ── Message (résultat de commande) ──
    if let Some(msg) = &tui.message {
        let message = Paragraph::new(msg.as_str())
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true });
        f.render_widget(message, chunks[1]);
    }

    // ── Logs (ou aide) ──
    if tui.help {
        let help = Paragraph::new(catalog.ops.tui_help)
            .block(Block::default().borders(Borders::ALL).title(format!("{} — ?", catalog.ops.tui_title)))
            .wrap(Wrap { trim: false });
        f.render_widget(help, chunks[2]);
    } else {
        let follow_label = if tui.follow {
            catalog.ops.tui_follow
        } else {
            catalog.ops.tui_paused
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                "{} — {} [{}]",
                catalog.ops.tui_logs_title,
                crate::paths::log_file().display(),
                follow_label
            ));
        let area = block.inner(chunks[2]);
        f.render_widget(block, chunks[2]);

        let visible = (area.height as usize).saturating_sub(1).max(1);
        let total = tui.lines.len();
        let end = total.saturating_sub(tui.scroll);
        let start = end.saturating_sub(visible);
        let lines: Vec<&str> = tui.lines[start..end].iter().map(String::as_str).collect();
        let text: Vec<ratatui::text::Line> = if lines.is_empty() {
            vec![ratatui::text::Line::from(catalog.ops.tui_log_waiting)]
        } else {
            lines.into_iter().map(ratatui::text::Line::from).collect()
        };
        let logs = Paragraph::new(text);
        f.render_widget(logs, area);
    }

    // ── Barre de commande ──
    let command = if tui.command_mode {
        format!("{} {}▏", catalog.ops.tui_prompt, tui.input)
    } else {
        format!("{} —", catalog.ops.tui_prompt)
    };
    let bar = Paragraph::new(command).style(Style::default().fg(if tui.command_mode {
        Color::Yellow
    } else {
        Color::DarkGray
    }));
    f.render_widget(bar, chunks[3]);

    // ── Aide clavier ──
    let keys = Paragraph::new(catalog.ops.tui_keys).style(Style::default().fg(Color::DarkGray));
    f.render_widget(keys, chunks[4]);
}
