//! Provider Nitrado (NitrAPI, token OAuth2 long-lived en Bearer).
//!
//! `targetId` = ID DU SERVICE Nitrado (celui du webinterface), PAS l'id
//! interne du gameserver : GET /services/{id}/gameservers renvoie
//! directement l'objet `data.gameserver` — un seul appel suffit.
//!
//! Écart documenté de l'API (comme en JS) :
//!   restart → POST /services/{id}/gameservers/restart
//!   stop    → POST /services/{id}/gameservers/stop
//!   start   → pas de start direct : POST /services/{id}/gameservers/games/start
//!             exige le paramètre `game` (identifiant court du jeu), résolu
//!             depuis le détail → DEUX appels pour start.
//!
//! Métriques : le détail n'expose ni CPU ni RAM — ils viennent de l'endpoint
//! séparé /services/{id}/gameservers/stats (séries `cpuUsage`, `memoryUsage`,
//! `currentPlayers` au format `[[valeur, timestamp], …]`). Si le payload
//! `data.stats` est présent, on retient le DERNIER point de chaque série.

use serde_json::{json, Value};

use super::{last_point, map_state, num, normalize_address, str_at, Auth, Meta, Provider, Rest, Status};
use crate::errors::ProviderError;
use std::time::Duration;

pub const META: Meta = Meta {
    name: "nitrado",
    display_name: "Nitrado",
    kind: "game",
    icon: "🎮",
    default_api_url: "https://api.nitrado.net",
};

const GET_TIMEOUT: Duration = Duration::from_secs(15);
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Statuts documentés de `data.gameserver.status` → vocabulaire commun.
/// `suspended` → stopped (service à réactiver depuis le site) ; les statuts
/// restants (restarting, guardian_locked, backup_*…) → unknown.
const STATUS_MAP: [(&str, &str); 5] = [
    ("started", "running"),
    ("stopped", "stopped"),
    ("suspended", "stopped"),
    ("starting", "starting"),
    ("stopping", "stopping"),
];

#[derive(Debug, Clone)]
pub struct Nitrado {
    rest: Rest,
}

impl Nitrado {
    pub fn new(api_key: &str, api_url: Option<&str>) -> Self {
        Self {
            rest: Rest::new(
                api_url.unwrap_or(META.default_api_url).to_string(),
                Auth::Bearer(api_key.to_string()),
                super::get_timeout(GET_TIMEOUT),
                POWER_TIMEOUT,
            ),
        }
    }
}

impl Provider for Nitrado {
    fn meta(&self) -> &'static Meta {
        &META
    }

    fn rest(&self) -> &Rest {
        &self.rest
    }

    async fn fetch_raw(&self, target: &str) -> Result<Value, ProviderError> {
        self.rest.get_json(&format!("/services/{target}/gameservers")).await
    }

    async fn send_power_raw(&self, target: &str, action: &str) -> Result<(), ProviderError> {
        match action {
            "restart" => {
                self.rest
                    .post_empty(&format!("/services/{target}/gameservers/restart"))
                    .await
                    .map(|_| ())
            }
            "stop" => {
                self.rest
                    .post_empty(&format!("/services/{target}/gameservers/stop"))
                    .await
                    .map(|_| ())
            }
            "start" => {
                // L'API ne propose pas de start direct sur le gameserver : le
                // démarrage passe par POST …/games/start qui exige `game`
                // (identifiant court du jeu), lu depuis le détail.
                let detail = self.rest.get_json(&format!("/services/{target}/gameservers")).await?;
                let game = str_at(&detail, &["data", "gameserver", "game"]).ok_or_else(|| {
                    ProviderError::other(
                        "Provider nitrado : impossible de démarrer — le détail ne fournit pas le champ `game` requis par POST /games/start.",
                    )
                })?;
                self.rest
                    .post_json(
                        &format!("/services/{target}/gameservers/games/start"),
                        &json!({ "game": game }),
                    )
                    .await
                    .map(|_| ())
            }
            other => Err(ProviderError::other(format!(
                "Action inconnue pour le provider nitrado : \"{other}\" (start|stop|restart)"
            ))),
        }
    }

    fn normalize(&self, raw: &Value) -> Status {
        normalize(raw)
    }
}

/// Normalise le corps d'une réponse Nitrado : `{status, data: {gameserver: {…}}}`.
pub fn normalize(raw: &Value) -> Status {
    let data = raw.get("data").and_then(|d| d.as_object());
    let gs = data
        .and_then(|d| d.get("gameserver"))
        .filter(|g| g.is_object())
        .unwrap_or(&Value::Null);
    let query = gs.get("query").and_then(|q| q.as_object());

    let raw_state = str_at(gs, &["status"])
        .unwrap_or_else(|| "unknown".to_string())
        .to_lowercase();
    let state = map_state(&raw_state, &STATUS_MAP, "unknown");

    // players : d'abord `query.player_current`, sinon dernier point de la
    // série `currentPlayers` des stats.
    let mut players = num(gs, &["query", "player_current"]);
    let mut cpu_pct = None;
    let mut ram_mb = None;
    if let Some(stats) = data.and_then(|d| d.get("stats")) {
        cpu_pct = last_point(stats, &["cpuUsage"]);
        ram_mb = last_point(stats, &["memoryUsage"]);
        if players.is_none() {
            players = last_point(stats, &["currentPlayers"]);
        }
    }

    // Adresse : query.server_ip/server_port (anciennes versions) > connect_ip
    // "ip:port" > ip/port à la racine du gameserver.
    let address = query
        .and_then(|_| {
            let ip = str_at(gs, &["query", "server_ip"]);
            let port = num(gs, &["query", "server_port"]);
            ip.map(|ip| super::Address {
                ip,
                port: port.map(|p| p as u32),
            })
        })
        .or_else(|| {
            str_at(gs, &["query", "connect_ip"]).and_then(|s| normalize_address(&Value::String(s)))
        })
        .or_else(|| {
            let ip = str_at(gs, &["ip"]);
            ip.map(|ip| super::Address {
                ip,
                port: num(gs, &["port"]).map(|p| p as u32),
            })
        });

    Status {
        name: str_at(gs, &["query", "server_name"]),
        state,
        raw_state,
        cpu_pct,
        ram_mb,
        ram_max_mb: None,      // non exposé par l'API Nitrado
        disk_mb: None,         // non exposé
        disk_max_mb: None,     // non exposé
        uptime_seconds: None,  // non exposé sous forme directe
        players,
        address,
        node: str_at(gs, &["location_id"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_statuses() {
        let norm = |raw: Value| Nitrado::new("k", None).normalize(&raw);
        assert_eq!(norm(json!({"data": {"gameserver": {"status": "started"}}})).state, "running");
        assert_eq!(norm(json!({"data": {"gameserver": {"status": "suspended"}}})).state, "stopped");
        assert_eq!(norm(json!({"data": {"gameserver": {"status": "stopping"}}})).state, "stopping");
        assert_eq!(norm(json!({"data": {"gameserver": {"status": "guardian_locked"}}})).state, "unknown");
    }

    #[test]
    fn reads_last_stat_points() {
        let s = Nitrado::new("k", None).normalize(&json!({
            "data": {
                "gameserver": {"status": "started", "query": {"player_current": null}},
                "stats": {
                    "cpuUsage": [[5.0, 1000], [42.5, 2000]],
                    "memoryUsage": [[100.0, 1000], [346.0, 2000]],
                    "currentPlayers": [[0, 1000], [3, 2000]]
                }
            }
        }));
        assert_eq!(s.cpu_pct, Some(42.5));
        assert_eq!(s.ram_mb, Some(346.0));
        assert_eq!(s.players, Some(3.0));
    }

    #[test]
    fn address_prefers_connect_ip() {
        let s = Nitrado::new("k", None).normalize(&json!({
            "data": {"gameserver": {"status": "started", "query": {"connect_ip": "play.nitrado.net:27015"}}}
        }));
        let a = s.address.unwrap();
        assert_eq!(a.ip, "play.nitrado.net");
        assert_eq!(a.port, Some(27015));
    }
}
