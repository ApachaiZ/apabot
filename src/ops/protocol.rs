//! Protocole du canal de contrôle (TCP sur 127.0.0.1, JSON lignes).
//!
//! Pourquoi un canal au lieu de simples signaux ? Les signaux Unix (SIGTERM)
//! n'existent pas sur Windows et ne transportent pas de réponse. Un petit
//! serveur TCP local — une connexion par commande, une réponse par ligne —
//! donne un arrêt/redémarrage PROPRES et identiques sur tous les OS, avec
//! authentification par jeton aléatoire (mode 0600 sur le fichier d'état).
//!
//! L'enfant (le bot) se connecte AUSSI au canal, avec `kind: child` : le
//! superviseur garde sa connexion ouverte et peut lui pousser `shutdown`.
//! C'est le mécanisme d'arrêt propre cross-platform (le bot coupe sa
//! gateway Discord puis se termine).

use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::ops::state::DaemonState;

/// Délai maximal d'attente d'une réponse du superviseur.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Réponse du superviseur à une commande client.
#[derive(Debug, Clone)]
pub struct Response {
    pub ok: bool,
    pub error: Option<String>,
    /// Présent pour `status` : instantané du superviseur.
    pub status: Option<StatusSnapshot>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StatusSnapshot {
    pub pid: u32,
    /// PID du processus bot (absent pendant la fenêtre de redémarrage).
    pub child_pid: Option<u32>,
    /// Uptime du PROCESSUS BOT actuel, en secondes.
    pub child_uptime_secs: u64,
    pub restarts: u32,
}

/// Envoie une commande au superviseur décrit par `state` et attend la
/// réponse (une seule ligne JSON). Erreur = chaîne lisible pour l'utilisateur.
pub async fn request(state: &DaemonState, cmd: &str) -> Result<Response, String> {
    let addr = format!("127.0.0.1:{}", state.port);
    let mut stream = tokio::time::timeout(REQUEST_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| "connection timeout".to_string())?
        .map_err(|e| e.to_string())?;

    let payload = json!({ "kind": "ctl", "token": state.token, "cmd": cmd }).to_string();
    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(payload.as_bytes()))
        .await
        .map_err(|_| "write timeout".to_string())?
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(b"\n"))
        .await
        .map_err(|_| "write timeout".to_string())?
        .map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    tokio::time::timeout(REQUEST_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|e| e.to_string())?;

    parse_response(&line)
}

/// Parse la ligne de réponse JSON du superviseur.
pub fn parse_response(line: &str) -> Result<Response, String> {
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("bad response ({e})"))?;
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let error = value
        .get("error")
        .and_then(Value::as_str)
        .map(String::from);
    let status = value
        .get("status")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok());
    Ok(Response { ok, error, status })
}

/// Vérifie qu'un superviseur décrit par `state` répond (un `ping`).
pub async fn is_alive(state: &DaemonState) -> bool {
    request(state, "ping").await.map(|r| r.ok).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ok_response_with_status() {
        let line = r#"{"ok":true,"status":{"pid":1,"child_pid":2,"child_uptime_secs":3,"restarts":4}}"#;
        let r = parse_response(line).unwrap();
        assert!(r.ok);
        assert!(r.error.is_none());
        let s = r.status.unwrap();
        assert_eq!(s.pid, 1);
        assert_eq!(s.child_pid, Some(2));
        assert_eq!(s.child_uptime_secs, 3);
        assert_eq!(s.restarts, 4);
    }

    #[test]
    fn parses_error_response() {
        let r = parse_response(r#"{"ok":false,"error":"nope"}"#).unwrap();
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("nope"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_response("not json").is_err());
    }
}
