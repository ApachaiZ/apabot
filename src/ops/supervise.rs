//! Le SUPERVISEUR : le cœur du mission control.
//!
//! Processus détaché lancé par `apabot start`, il fait le travail que
//! faisait PM2 :
//! 1. il démarre le bot (`apabot run --non-interactive`) en enfant ;
//! 2. il capture stdout/stderr de l'enfant dans le fichier de log partagé ;
//! 3. il applique la politique de redémarrage (5 s d'attente, 10 relances
//!    max, compteur remis à zéro après 30 s stables, exit 78 = abandon) ;
//! 4. il sert le CANAL DE CONTRÔLE (TCP 127.0.0.1, JSON lignes, jeton
//!    aléatoire) : `ping`, `status`, `stop`, `restart` ;
//! 5. il pousse `shutdown` à l'enfant via ce même canal : arrêt PROPRE sur
//!    tous les OS, sans signal (Windows n'a pas de SIGTERM).
//!
//! L'état vivant est écrit dans `.config.d/daemon.json` (voir `state.rs`).

use rand::RngCore;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::i18n::{fill, Catalog};
use crate::logger;
use crate::ops::state;

/// Délai avant chaque relance automatique (parité PM2 `restart_delay`).
pub const RESTART_DELAY: Duration = Duration::from_secs(5);
/// Nombre maximal de relances consécutives (parité PM2 `max_restarts`).
pub const MAX_RESTARTS: u32 = 10;
/// Durée de vie minimale pour « stabiliser » le compteur de relances
/// (parité PM2 `min_uptime`).
pub const STABLE_UPTIME: Duration = Duration::from_secs(30);
/// Délai accordé au bot pour s'arrêter proprement avant le kill forcé
/// (parité PM2 `kill_timeout`).
pub const KILL_TIMEOUT: Duration = Duration::from_secs(10);

// ── Politique de redémarrage (pure, testable) ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartAction {
    /// Ne pas relancer (configuration ou trop de relances).
    GiveUp,
    /// Relancer, avec le nouveau compteur de relances.
    Restart { restarts: u32 },
}

/// Décision après la sortie de l'enfant, en parité stricte avec PM2 :
/// - exit 78 (EX_CONFIG) : jamais de relance — un `.env` incomplet ne se
///   répare pas en boucle ;
/// - durée de vie ≥ 30 s : compteur remis à zéro ;
/// - sinon compteur +1, abandon au-delà de `MAX_RESTARTS`.
pub fn restart_after(exit_code: Option<i32>, uptime: Duration, restarts: u32) -> RestartAction {
    if exit_code == Some(78) {
        return RestartAction::GiveUp;
    }
    if uptime >= STABLE_UPTIME {
        return RestartAction::Restart { restarts: 0 };
    }
    if restarts + 1 > MAX_RESTARTS {
        return RestartAction::GiveUp;
    }
    RestartAction::Restart {
        restarts: restarts + 1,
    }
}

// ── État partagé superviseur ↔ serveur de contrôle ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupCmd {
    Stop,
    Restart,
}

/// Infos sur l'enfant courant, partagées avec le serveur de contrôle.
#[derive(Default)]
struct ChildInfo {
    pid: Option<u32>,
    restarts: u32,
    started: Option<Instant>,
}

struct Control {
    cmd_tx: mpsc::UnboundedSender<SupCmd>,
    info: Arc<Mutex<ChildInfo>>,
    /// Demi-canal d'ÉCRITURE vers l'enfant (posé par le hello de l'enfant).
    child_conn: Arc<Mutex<Option<OwnedWriteHalf>>>,
    token: String,
    supervisor_pid: u32,
    catalog: &'static Catalog,
}

// ── Serveur de contrôle ────────────────────────────────────────────────────

async fn control_server(listener: TcpListener, ctl: Arc<Control>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let ctl = Arc::clone(&ctl);
                tokio::spawn(async move { handle_conn(stream, ctl).await });
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

async fn handle_conn(mut stream: TcpStream, ctl: Arc<Control>) {
    // Une ligne d'entrée : JSON `{kind, token, cmd?}`.
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        if reader.read_line(&mut line).await.is_err() {
            return;
        }
    }
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return;
    };
    let token_ok = value.get("token").and_then(Value::as_str) == Some(ctl.token.as_str());
    if !token_ok {
        let _ = reply(&mut stream, &json!({ "ok": false, "error": "bad token" })).await;
        return;
    }
    match value.get("kind").and_then(Value::as_str) {
        Some("ctl") => handle_ctl(value, &mut stream, &ctl).await,
        Some("child") => handle_child(stream, &ctl).await,
        _ => {}
    }
}

async fn reply(stream: &mut TcpStream, value: &Value) -> std::io::Result<()> {
    let mut payload = value.to_string();
    payload.push('\n');
    stream.write_all(payload.as_bytes()).await
}

async fn handle_ctl(value: Value, stream: &mut TcpStream, ctl: &Control) {
    let cmd = value.get("cmd").and_then(Value::as_str).unwrap_or("");
    match cmd {
        "ping" => {
            let _ = reply(stream, &json!({ "ok": true })).await;
        }
        "status" => {
            // Le MutexGuard est !Send : on extrait les valeurs AVANT le
            // premier await (la réponse réseau).
            let (child_pid, uptime, restarts) = {
                let info = ctl.info.lock().unwrap();
                (
                    info.pid,
                    info.started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                    info.restarts,
                )
            };
            let _ = reply(
                stream,
                &json!({
                    "ok": true,
                    "status": {
                        "pid": ctl.supervisor_pid,
                        "child_pid": child_pid,
                        "child_uptime_secs": uptime,
                        "restarts": restarts,
                    }
                }),
            )
            .await;
        }
        "stop" => {
            let _ = reply(stream, &json!({ "ok": true })).await;
            let _ = ctl.cmd_tx.send(SupCmd::Stop);
        }
        "restart" => {
            let _ = reply(stream, &json!({ "ok": true })).await;
            let _ = ctl.cmd_tx.send(SupCmd::Restart);
        }
        other => {
            let _ = reply(
                stream,
                &json!({ "ok": false, "error": format!("unknown command: {other}") }),
            )
            .await;
        }
    }
}

/// Connexion de l'ENFANT : il s'annonce avec le jeton, on garde sa moitié
/// d'écriture pour pouvoir lui pousser `shutdown`. On lit jusqu'à EOF sans
/// rien faire des données — la connexion elle-même est le signal de vie.
async fn handle_child(stream: TcpStream, ctl: &Control) {
    logger::info(ctl.catalog.ops.sup_child_connected);
    let (read, write) = stream.into_split();
    *ctl.child_conn.lock().unwrap() = Some(write);
    let mut reader = BufReader::new(read);
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf).await.is_err() {
            break;
        }
    }
    // Pas de nettoyage ici : la connexion de l'enfant suivant écrasera la
    // moitié d'écriture — la nettoyer ici pourrait effacer celle d'un
    // enfant plus récent déjà enregistré.
}

// ── Signaux (portables) ──────────────────────────────────────────────

/// Enveloppe les signaux d'arrêt : SIGTERM/SIGINT sous Unix, Ctrl-C
/// ailleurs. Un signal reçu vaut une commande `stop`.
struct Signals {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    fn new() -> Self {
        #[cfg(unix)]
        {
            let term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            let int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).ok();
            Self { term, int }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Se résout au premier signal d'arrêt.
    async fn wait(&mut self) {
        #[cfg(unix)]
        {
            let term = async {
                if let Some(s) = self.term.as_mut() {
                    let _ = s.recv().await;
                }
            };
            let int = async {
                if let Some(s) = self.int.as_mut() {
                    let _ = s.recv().await;
                }
            };
            tokio::pin!(term, int);
            tokio::select! {
                _ = &mut term => {},
                _ = &mut int => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

// ── Cycle de vie du superviseur ────────────────────────────────────────────

/// Point d'entrée du sous-processus `supervise`. Retourne le code de sortie :
/// 0 = arrêt propre, 78 = config du bot invalide, 1 = épuisement des
/// relances ou échec interne.
pub async fn supervise(catalog: &'static Catalog) -> i32 {
    logger::init();

    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            logger::error(format!("Could not open the control channel: {e}"));
            return 1;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);

    // Jeton aléatoire : seul détenteur = les processus qui lisent
    // `.config.d/daemon.json` (mode 0600).
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();

    let st = state::DaemonState {
        pid: std::process::id(),
        port,
        token: token.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = state::write(&st) {
        logger::error(format!("Could not write daemon state: {e}"));
        return 1;
    }
    logger::info(fill(
        catalog.ops.sup_start,
        &[("pid", &st.pid.to_string())],
    ));

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SupCmd>();
    let info = Arc::new(Mutex::new(ChildInfo::default()));
    let child_conn: Arc<Mutex<Option<OwnedWriteHalf>>> = Arc::new(Mutex::new(None));
    let ctl = Arc::new(Control {
        cmd_tx: cmd_tx.clone(),
        info: Arc::clone(&info),
        child_conn: Arc::clone(&child_conn),
        token: token.clone(),
        supervisor_pid: st.pid,
        catalog,
    });
    tokio::spawn(control_server(listener, ctl));

    // Signaux → équivalent d'un Stop (parité avec le kill_timeout de PM2 :
    // le bot a le temps de couper sa gateway).
    let mut signals = Signals::new();

    let mut restarts: u32 = 0;
    let mut exit_code: i32 = 0;

    'main: loop {
        // ── Lancement de l'enfant ──
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                logger::error(fill(catalog.ops.sup_child_spawn_failed, &[("error", &e.to_string())]));
                exit_code = 1;
                break;
            }
        };
        let mut cmd = Command::new(exe);
        cmd.arg("run")
            .arg("--non-interactive")
            .env("APABOT_DAEMON_PORT", port.to_string())
            .env("APABOT_DAEMON_TOKEN", &token)
            .env("APABOT_DAEMON_CHILD", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Les chemins du bot sont relatifs au répertoire courant : on fige
        // celui du lancement de `start`.
        if let Ok(cwd) = std::env::current_dir() {
            cmd.current_dir(cwd);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                logger::error(fill(catalog.ops.sup_child_spawn_failed, &[("error", &e.to_string())]));
                match restart_after(None, Duration::ZERO, restarts) {
                    RestartAction::Restart { restarts: r } => {
                        restarts = r;
                        if !sleep_or_cmd(&mut cmd_rx, &mut signals, RESTART_DELAY).await {
                            break;
                        }
                        continue;
                    }
                    RestartAction::GiveUp => {
                        exit_code = 1;
                        break;
                    }
                }
            }
        };

        {
            let mut guard = info.lock().unwrap();
            guard.pid = child.id();
            guard.restarts = restarts;
            guard.started = Some(Instant::now());
        }
        let spawned_at = Instant::now();
        logger::info(format!(
            "Bot process {} spawned.",
            child.id().unwrap_or_default()
        ));

        // Capture stdout/stderr de l'enfant dans le fichier de log partagé.
        if let Some(stdout) = child.stdout.take() {
            pipe_to_log(BufReader::new(stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            pipe_to_log(BufReader::new(stderr));
        }

        // ── Attente : sortie de l'enfant, commande, ou signal ──
        enum Event {
            ChildExited(Option<std::process::ExitStatus>),
            Cmd(SupCmd),
        }
        let event = tokio::select! {
            status = child.wait() => Event::ChildExited(status.ok()),
            cmd = cmd_rx.recv() => match cmd {
                Some(c) => Event::Cmd(c),
                None => Event::Cmd(SupCmd::Stop), // canal fermé : on arrête tout
            },
            _ = signals.wait() => Event::Cmd(SupCmd::Stop),
        };

        match event {
            Event::ChildExited(status) => {
                {
                    let mut guard = info.lock().unwrap();
                    guard.pid = None;
                    guard.started = None;
                }
                let code = status.as_ref().and_then(|s| s.code());
                // Libellé fidèle : `code N` ou `signal N` (un panic avec
                // `panic = "abort"` donnait SIGABRT, affiché à tort « code 0 »
                // car `ExitStatus::code()` vaut `None` pour une mort par
                // signal).
                let label = status
                    .as_ref()
                    .map(exit_label)
                    .unwrap_or_else(|| "unknown status".to_string());
                if code == Some(78) {
                    logger::error(catalog.ops.sup_config_exit);
                    exit_code = 78;
                    break;
                }
                let uptime = spawned_at.elapsed();
                match restart_after(code, uptime, restarts) {
                    RestartAction::GiveUp => {
                        logger::error(fill(catalog.ops.sup_max_restarts, &[("n", &restarts.to_string())]));
                        exit_code = 1;
                        break;
                    }
                    RestartAction::Restart { restarts: r } => {
                        if r == 0 {
                            logger::warn(fill(
                                catalog.ops.sup_child_exit_stable,
                                &[("code", &label)],
                            ));
                        } else {
                            logger::warn(fill(
                                catalog.ops.sup_child_exit,
                                &[
                                    ("code", &label),
                                    ("n", &r.to_string()),
                                    ("max", &MAX_RESTARTS.to_string()),
                                ],
                            ));
                        }
                        restarts = r;
                        if !sleep_or_cmd(&mut cmd_rx, &mut signals, RESTART_DELAY).await {
                            break;
                        }
                    }
                }
            }
            Event::Cmd(cmd) => match cmd {
                SupCmd::Restart => {
                    logger::info(catalog.ops.sup_restart_requested);
                    shutdown_child(&child_conn, &mut child, catalog).await;
                    {
                        let mut guard = info.lock().unwrap();
                        guard.pid = None;
                        guard.started = None;
                    }
                    // Pas de backoff pour un redémarrage demandé.
                }
                SupCmd::Stop => {
                    logger::info(catalog.ops.sup_stopping);
                    shutdown_child(&child_conn, &mut child, catalog).await;
                    exit_code = 0;
                    break 'main;
                }
            },
        }
    }

    // Sortie : un éventuel enfant encore vivant est arrêté, l'état nettoyé.
    state::remove();
    logger::info(format!("Supervisor exiting (code {exit_code})."));
    exit_code
}

/// Libellé lisible d'une sortie d'enfant : `code N`, ou `signal N` sur une
/// mort par signal (SIGABRT, SIGKILL… — `ExitStatus::code()` vaut alors
/// `None`, l'ancien code les affichait à tort comme « code 0 »).
fn exit_label(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal {sig}");
        }
    }
    status
        .code()
        .map(|c| format!("code {c}"))
        .unwrap_or_else(|| "unknown status".to_string())
}

/// Demande d'arrêt propre à l'enfant : message `shutdown` sur le canal
/// (cross-platform), puis attente jusqu'à `KILL_TIMEOUT`, puis kill forcé.
async fn shutdown_child(
    child_conn: &Arc<Mutex<Option<OwnedWriteHalf>>>,
    child: &mut Child,
    catalog: &Catalog,
) {
    // La garde du mutex ne doit pas traverser les awaits (clippy) : on la
    // libère avant d'écrire sur le socket.
    let writer = { child_conn.lock().unwrap().take() };
    if let Some(mut writer) = writer {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            writer.write_all(b"{\"cmd\":\"shutdown\"}\n"),
        )
        .await;
        let _ = writer.shutdown().await;
    }
    match tokio::time::timeout(KILL_TIMEOUT, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            logger::warn(catalog.ops.sup_force_kill);
            let _ = child.kill().await;
        }
    }
}

/// Sommeil de backoff interrompu par une commande ou un signal.
/// `false` = STOP demandé (le superviseur doit s'arrêter) ; `true` = on
/// peut relancer (fin de backoff, ou un restart a été demandé).
async fn sleep_or_cmd(
    cmd_rx: &mut mpsc::UnboundedReceiver<SupCmd>,
    signals: &mut Signals,
    duration: Duration,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => true,
        cmd = cmd_rx.recv() => match cmd {
            Some(SupCmd::Stop) | None => false,
            Some(SupCmd::Restart) => true,
        },
        _ = signals.wait() => false,
    }
}

/// Recopie ligne par ligne la sortie de l'enfant dans le log (fichier
/// uniquement, sans reformatage : les panics et backtraces restent lisibles
/// à côté des lignes du logger).
fn pipe_to_log<R>(mut reader: R)
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let l = line.trim_end_matches(['\n', '\r']).to_string();
                    if !l.is_empty() {
                        logger::passthrough(l);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_78_never_restarts() {
        assert_eq!(
            restart_after(Some(78), Duration::ZERO, 0),
            RestartAction::GiveUp
        );
    }

    #[test]
    fn stable_run_resets_the_counter() {
        assert_eq!(
            restart_after(Some(1), STABLE_UPTIME, 7),
            RestartAction::Restart { restarts: 0 }
        );
    }

    #[test]
    fn consecutive_crashes_increment_until_the_limit() {
        assert_eq!(
            restart_after(Some(1), Duration::from_secs(2), 0),
            RestartAction::Restart { restarts: 1 }
        );
        assert_eq!(
            restart_after(Some(1), Duration::from_secs(2), MAX_RESTARTS),
            RestartAction::GiveUp
        );
    }
}
