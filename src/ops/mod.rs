//! Mission control : les commandes d'exploitation du binaire.
//!
//! `apabot` n'est plus seulement le bot : il est aussi son propre
//! superviseur, sans PM2 ni systemd (voir `supervise.rs` pour le mécanisme).
//! Ce module distribue les sous-commandes :
//!
//! ```text
//! apabot                       → bot au premier plan (comportement historique)
//! apabot run                   → idem, forme explicite (utilisée par le superviseur)
//! apabot start|stop|restart    → cycle de vie du daemon supervisé
//! apabot status                → état du daemon
//! apabot logs [--lines N] [--no-follow] → dernier log + suivi en direct
//! apabot mission-control       → TUI interactif (clavier + commandes)
//! apabot systemd install|remove → gestion systemd optionnelle (Linux,
//!                                     avec confirmations, voir systemd.rs)
//! ```

pub mod config;
pub mod install;
pub mod protocol;
pub mod spawn;
pub mod state;
pub mod supervise;
pub mod systemd;
pub mod tail;

use std::time::Duration;

use crate::i18n::{fill, Catalog};
use crate::paths;

/// Sous-commandes connues du binaire (la première position de argv).
pub const SUBCOMMANDS: [&str; 15] = [
    "run",
    "start",
    "stop",
    "restart",
    "status",
    "logs",
    "mission-control",
    "monitor",
    "systemd",
    "help",
    "supervise",
    "install",
    "reinstall",
    "uninstall",
    "config",
];

/// Aiguillage du premier argument. `Some(code)` = sous-commande traitée,
/// sortir avec ce code. `None` = lancer le bot au premier plan (aucune
/// sous-commande, ou `run`).
pub async fn dispatch(sub: &str, args: &[String]) -> Option<i32> {
    // Le superviseur résout sa langue via APABOT_LANG (posé par `start`) ;
    // les autres commandes via les arguments/environnement comme le bot.
    let lang = if sub == "supervise" {
        supervisor_lang()
    } else {
        crate::i18n::resolve_initial_lang(args)
    };
    let catalog = crate::i18n::catalog(lang);

    match sub {
        "start" => match cmd_start(lang, catalog).await {
            // Après le démarrage, on montre la carte d'état du daemon —
            // le même visuel que la bannière du lancement en premier plan.
            Ok(msg) => {
                println!("{msg}");
                println!();
                println!("{}", cmd_status(catalog).await);
                Some(0)
            }
            Err(msg) => Some(print_result(Err(msg))),
        },
        "stop" => Some(print_result(cmd_stop(catalog).await)),
        "restart" => Some(print_result(cmd_restart(lang, catalog).await)),
        "status" => {
            println!("{}", cmd_status(catalog).await);
            Some(0)
        }
        "logs" => Some(cmd_logs(args, catalog).await),
        "mission-control" | "monitor" => Some(run_tui(lang, catalog).await),
        "systemd" => Some(systemd::cmd_systemd(args, catalog)),
        "install" | "reinstall" | "uninstall" => Some(install::cmd_install(args, catalog).await),
        "config" => Some(config::cmd_config(args, catalog).await),
        "help" => {
            println!("{}", catalog.ops.usage);
            Some(0)
        }
        "supervise" => Some(supervise::supervise(catalog).await),
        // `run` et tout le reste (dont les flags comme --non-interactive) :
        // le bot démarre au premier plan.
        _ => None,
    }
}

/// Langue du superviseur : `APABOT_LANG` (posé par `start`, suit la langue du
/// CLI) > `BOT_LANGUAGE` > `en`.
fn supervisor_lang() -> &'static str {
    if let Ok(lang) = std::env::var("APABOT_LANG") {
        if lang == "fr" || lang == "en" {
            return match lang.as_str() {
                "fr" => "fr",
                _ => "en",
            };
        }
    }
    crate::i18n::resolve_initial_lang(&[])
}

/// Affiche le résultat d'une commande (Ok → stdout, Err → stderr) et
/// renvoie le code de sortie.
fn print_result(result: Result<String, String>) -> i32 {
    match result {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(msg) => {
            eprintln!("{msg}");
            1
        }
    }
}

// ── Commandes ──────────────────────────────────────────────────────────────

/// `start` : lance le superviseur détaché, ou signale qu'il tourne déjà.
/// `lang` est transmise au superviseur (messages du daemon dans la langue
/// de l'appelant).
pub async fn cmd_start(lang: &str, catalog: &Catalog) -> Result<String, String> {
    let launching = catalog.ops.start_launching;
    let mut prefix = String::new();
    if let Some(st) = state::read() {
        if protocol::is_alive(&st).await {
            return Ok(fill(catalog.ops.start_already, &[("pid", &st.pid.to_string())]));
        }
        if state::pid_alive(st.pid) {
            return Err(fill(catalog.ops.not_responding, &[("pid", &st.pid.to_string())]));
        }
        // État obsolète (superviseur mort) : nettoyage puis démarrage neuf.
        state::remove();
        prefix = fill(catalog.ops.start_stale, &[("pid", &st.pid.to_string())]);
    }

    spawn::spawn_supervisor(lang).map_err(|e| fill(
        catalog.ops.sup_child_spawn_failed,
        &[("error", &e.to_string())],
    ))?;

    // Le superviseur écrit son état juste après le bind : on l'attend
    // (max 3 s) pour pouvoir annoncer le pid réel.
    let mut st = None;
    for _ in 0..30 {
        if let Some(s) = state::read() {
            st = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    match st {
        Some(s) => {
            let msg = fill(catalog.ops.start_ok, &[("pid", &s.pid.to_string())]);
            let msg = if prefix.is_empty() {
                format!("{launching}\n{msg}")
            } else {
                format!("{prefix}\n{launching}\n{msg}")
            };
            Ok(msg)
        }
        None => {
            // Le superviseur a pu démarrer ET déjà se terminer entre deux
            // sondages (exit 78 du bot en ~1 ms) : la dernière ligne du log
            // explique alors la cause exacte.
            let mut msg =
                "The supervisor did not report its state — see logs/apabot.log.".to_string();
            if let Ok(raw) = std::fs::read_to_string(paths::log_file()) {
                if let Some(last) = raw.lines().rev().find(|l| !l.trim().is_empty()) {
                    msg.push_str(&format!("\nLast log line: {last}"));
                }
            }
            Err(msg)
        }
    }
}

/// `stop` : arrêt propre du daemon (message `shutdown` sur le canal de
/// contrôle, kill forcé après le délai de grâce côté superviseur).
pub async fn cmd_stop(catalog: &Catalog) -> Result<String, String> {
    let Some(st) = state::read() else {
        return Ok(catalog.ops.not_running.to_string());
    };
    match protocol::request(&st, "stop").await {
        Ok(r) if r.ok => Ok(catalog.ops.stop_ok.to_string()),
        Ok(r) => Err(fill(
            catalog.ops.stop_failed,
            &[("error", &r.error.unwrap_or_else(|| "no error given".into()))],
        )),
        Err(_) => {
            if state::pid_alive(st.pid) {
                Err(fill(catalog.ops.not_responding, &[("pid", &st.pid.to_string())]))
            } else {
                // Superviseur déjà mort : état obsolète nettoyé.
                state::remove();
                Ok(format!(
                    "{}\n{}",
                    fill(catalog.ops.start_stale, &[("pid", &st.pid.to_string())]),
                    catalog.ops.stop_ok
                ))
            }
        }
    }
}

/// `restart` : redémarrage du bot via le superviseur ; si le daemon est
/// arrêté, on le démarre (comportement PM2).
pub async fn cmd_restart(lang: &str, catalog: &Catalog) -> Result<String, String> {
    let Some(st) = state::read() else {
        let start = cmd_start(lang, catalog).await?;
        return Ok(format!("{}\n{start}", catalog.ops.restart_started));
    };
    match protocol::request(&st, "restart").await {
        Ok(r) if r.ok => Ok(catalog.ops.restart_ok.to_string()),
        Ok(r) => Err(fill(
            catalog.ops.stop_failed,
            &[("error", &r.error.unwrap_or_else(|| "no error given".into()))],
        )),
        Err(_) => {
            if state::pid_alive(st.pid) {
                Err(fill(catalog.ops.not_responding, &[("pid", &st.pid.to_string())]))
            } else {
                state::remove();
                let start = cmd_start(lang, catalog).await?;
                Ok(format!("{}\n{start}", catalog.ops.restart_started))
            }
        }
    }
}

/// Instantané pour `status` et pour le TUI.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub running: bool,
    pub pid: Option<u32>,
    pub child_pid: Option<u32>,
    pub uptime_secs: u64,
    pub restarts: u32,
}

pub async fn status_snapshot() -> Snapshot {
    let Some(st) = state::read() else {
        return Snapshot::default();
    };
    match protocol::request(&st, "status").await {
        Ok(r) if r.ok => {
            let s = r.status.unwrap_or(protocol::StatusSnapshot {
                pid: st.pid,
                child_pid: None,
                child_uptime_secs: 0,
                restarts: 0,
            });
            Snapshot {
                running: true,
                pid: Some(s.pid),
                child_pid: s.child_pid,
                uptime_secs: s.child_uptime_secs,
                restarts: s.restarts,
            }
        }
        _ => Snapshot {
            running: false,
            pid: Some(st.pid),
            ..Snapshot::default()
        },
    }
}

/// `status` : une CARTE encadrée du même style que la bannière de
/// démarrage (cyan sur terminal), avec l'état du daemon — ou l'état
/// « arrêté » / « ne répond pas » accompagné d'une ligne d'explication.
pub async fn cmd_status(catalog: &Catalog) -> String {
    let snapshot = status_snapshot().await;
    let mut extra: Option<String> = None;

    let mut lines = vec![catalog.ops.status_card_title.to_string()];
    if snapshot.running {
        let bot = snapshot
            .child_pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!("📡  {}: {}", catalog.ops.status_state, catalog.ops.tui_running));
        lines.push(format!("🧵  {}: {}", catalog.ops.tui_pid, snapshot.pid.unwrap_or(0)));
        lines.push(format!("🤖  {}: {}", catalog.ops.tui_bot, bot));
        lines.push(format!("⏱   {}: {}", catalog.ops.tui_uptime, human_duration(snapshot.uptime_secs)));
        lines.push(format!("🔁  {}: {}", catalog.ops.tui_restarts, snapshot.restarts));
    } else if let Some(pid) = snapshot.pid {
        // État trouvé mais le daemon ne répond pas au ping : soit il est
        // occupé (vivant → on N'Y TOUCHE PAS, le supprimer permettrait un
        // second superviseur), soit il est mort (obsolète → nettoyage).
        lines.push(format!("📡  {}: {}", catalog.ops.status_state, catalog.ops.status_stopped));
        if state::pid_alive(pid) {
            extra = Some(fill(catalog.ops.not_responding, &[("pid", &pid.to_string())]));
        } else {
            state::remove();
            extra = Some(fill(catalog.ops.start_stale, &[("pid", &pid.to_string())]));
        }
    } else {
        lines.push(format!("📡  {}: {}", catalog.ops.status_state, catalog.ops.status_stopped));
    }

    let card = crate::logger::paint(&crate::logger::boxed_text(&lines));
    match extra {
        Some(detail) => format!("{card}\n{detail}"),
        None => card,
    }
}

/// `logs` : dernières lignes puis suivi en direct (Ctrl-C pour sortir).
async fn cmd_logs(args: &[String], catalog: &Catalog) -> i32 {
    let mut lines = 20usize;
    let mut follow = true;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--no-follow" => follow = false,
            "--lines" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse() {
                        lines = n;
                    }
                    i += 1;
                }
            }
            a if a.starts_with("--lines=") => {
                if let Ok(n) = a["--lines=".len()..].parse() {
                    lines = n;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let path = paths::log_file();
    // En-tête encadré du même style que la bannière de démarrage, puis le
    // flux brut des lignes (grep-friendly, déjà horodatées par le logger).
    let header = crate::logger::boxed_text(&[
        catalog.ops.logs_title.to_string(),
        format!("📁  {}: {}", catalog.ops.logs_file, path.display()),
    ]);
    println!("{}", crate::logger::paint(&header));
    if !path.exists() {
        println!("{}", catalog.ops.log_none);
        return 0;
    }
    let (initial, offset) = tail::tail_lines(&path, lines);
    for line in &initial {
        println!("{line}");
    }
    if !follow {
        return 0;
    }
    let mut follower = tail::Follower::new(offset);
    loop {
        for line in follower.poll(&path) {
            println!("{line}");
        }
        tokio::time::sleep(tail::POLL_INTERVAL).await;
    }
}

/// TUI mission control : erreur claire si le terminal ne le permet pas.
async fn run_tui(lang: &'static str, catalog: &'static Catalog) -> i32 {
    match crate::tui::run(lang, catalog).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mission-control needs a real terminal: {e}");
            1
        }
    }
}

// ── Petits outils ──────────────────────────────────────────────────────────

/// Durée lisible (`3h 02m`, `45s`, `2j 01h`…).
pub fn human_duration(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    match (d, h, m, s) {
        (0, 0, 0, _) => format!("{s}s"),
        (0, 0, _, _) => format!("{m}m {s:02}s"),
        (0, _, _, _) => format!("{h}h {m:02}m"),
        _ => format!("{d}j {h:02}h"),
    }
}

/// Future résolue quand le SUPERVISEUR demande l'arrêt (mode daemon),
/// jamais résolue en premier plan. La connexion d'enregistrement a lieu au
/// premier poll — donc tôt dans `main` — pour qu'un `stop` reste propre
/// même pendant le démarrage du bot. C'est l'équivalent cross-platform du
/// SIGTERM : Windows n'a pas de signal, le message `shutdown` sur le canal
/// de contrôle le remplace partout.
pub async fn daemon_shutdown() {
    let Ok(port) = std::env::var("APABOT_DAEMON_PORT") else {
        std::future::pending::<()>().await;
        return;
    };
    let token = std::env::var("APABOT_DAEMON_TOKEN").unwrap_or_default();
    let addr = format!("127.0.0.1:{port}");

    // Retry court : le superviseur écoute AVANT de nous lancer, mais une
    // rafale au boot ne coûte rien.
    let mut stream = None;
    for _ in 0..20 {
        match tokio::net::TcpStream::connect(&addr).await {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    let Some(mut stream) = stream else {
        // Superviseur parti : on vit en premier plan (Ctrl-C classique).
        return;
    };

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let hello = serde_json::json!({ "kind": "child", "token": token }).to_string();
    if stream.write_all(hello.as_bytes()).await.is_err() {
        return;
    }
    if stream.write_all(b"\n").await.is_err() {
        return;
    }
    let mut reader = tokio::io::BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.is_err() {
            return;
        }
        if line.trim().contains("shutdown") {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_duration_formats_all_scales() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(125), "2m 05s");
        assert_eq!(human_duration(7320), "2h 02m");
        assert_eq!(human_duration(90000), "1j 01h");
    }
}
